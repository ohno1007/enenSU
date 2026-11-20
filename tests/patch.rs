use android_bootimg::{
    cpio::{Cpio, CpioEntry},
    parser::BootImage,
};
use flate2::{Compression, write::GzEncoder};
use object::{Architecture, BinaryFormat, Endianness, SectionKind, write::Object};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output},
};
use tempfile::tempdir;

fn module(id: &str, depends: &str) -> Vec<u8> {
    let mut elf = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Little);
    let info = elf.add_section(Vec::new(), b".modinfo".to_vec(), SectionKind::ReadOnlyData);
    let fields = format!("name={id}\0vermagic=6.6-fixture\0depends={depends}\0");
    elf.append_section_data(info, fields.as_bytes(), 1);
    elf.write().unwrap()
}

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cpio = Cpio::new();
    for &(name, bytes) in files {
        cpio.add(name, CpioEntry::regular(0o644, Box::new(bytes.to_vec())))
            .unwrap();
    }
    let mut result = Vec::new();
    cpio.dump(&mut result).unwrap();
    result
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(bytes).unwrap();
    gz.finish().unwrap()
}

fn word(buf: &mut [u8], offset: usize, value: usize) {
    buf[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
}

// Use the Android vendor_boot on-disk layout independently of the production repacker.
fn vendor(version: u8, fragments: &[(&str, usize, &[u8])]) -> Vec<u8> {
    const PAGE: usize = 4096;
    let mut bytes = vec![0; PAGE];
    bytes[..8].copy_from_slice(b"VNDRBOOT");
    word(&mut bytes, 8, version.into());
    word(&mut bytes, 12, PAGE);
    word(&mut bytes, 2096, if version == 3 { 2112 } else { 2128 });
    let mut entries = Vec::new();
    let mut size = 0;
    for (name, kind, ramdisk) in fragments {
        let mut entry = [0; 108];
        word(&mut entry, 0, ramdisk.len());
        word(&mut entry, 4, size);
        word(&mut entry, 8, *kind);
        entry[12..12 + name.len()].copy_from_slice(name.as_bytes());
        word(&mut entry, 44, 37);
        entries.extend_from_slice(&entry);
        bytes.extend_from_slice(ramdisk);
        size += ramdisk.len();
    }
    word(&mut bytes, 24, size);
    bytes.resize(bytes.len().next_multiple_of(PAGE), 0);
    if version == 4 {
        word(&mut bytes, 2112, entries.len());
        word(&mut bytes, 2116, fragments.len());
        word(&mut bytes, 2120, 108);
        bytes.extend(entries);
        bytes.resize(bytes.len().next_multiple_of(PAGE), 0);
    }
    bytes
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_enensu"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

fn patch(root: &Path, input: &str, output: &str, selection: &[&str]) -> Vec<u8> {
    let mut args = vec!["patch", input, "-o", output];
    args.extend_from_slice(selection);
    let run = invoke(root, &args);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::read(root.join(output)).unwrap()
}

#[test]
fn patch_changes_only_selected_files_and_is_idempotent() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let stock = module("stock", "");
    let raw = archive(&[
        ("init", b"stock init"),
        ("lib/modules/stock.ko", &stock),
        ("lib/modules/modules.dep", b"stock.ko:"),
        ("lib/modules/modules.load", b"# vendor list"),
    ]);
    let image = vendor(3, &[("", 0, &gzip(&raw))]);
    fs::write(dir.join("vendor.img"), &image).unwrap();
    let helper = module("helper", "stock");
    let sensor = module("sensor", "helper");
    fs::write(dir.join("helper.ko"), &helper).unwrap();
    fs::write(dir.join("sensor.ko"), &sensor).unwrap();
    let selection = [
        "--module",
        "sensor.ko",
        "--module",
        "helper.ko",
        "--module",
        "sensor.ko",
    ];
    assert_eq!(patch(dir, "vendor.img", "copy.img", &[]), image);
    let changed = patch(dir, "vendor.img", "patched.img", &selection);
    let parsed = BootImage::parse(&changed).unwrap();
    let mut bytes = Vec::new();
    parsed
        .get_blocks()
        .get_ramdisk()
        .unwrap()
        .dump(&mut bytes, false)
        .unwrap();
    assert!(bytes.starts_with(&raw));
    let overlay = Cpio::load_from_data(&bytes[raw.len()..]).unwrap();
    let files: BTreeSet<_> = overlay
        .entries()
        .keys()
        .map(String::as_str)
        .filter(|s| !matches!(*s, "lib" | "lib/modules"))
        .collect();
    assert_eq!(
        files,
        BTreeSet::from([
            "lib/modules/helper.ko",
            "lib/modules/sensor.ko",
            "lib/modules/modules.dep",
            "lib/modules/modules.load"
        ])
    );
    let entry = |name| overlay.entry_by_name(name).unwrap().data().unwrap();
    assert_eq!(entry("lib/modules/sensor.ko"), sensor);
    assert_eq!(entry("lib/modules/helper.ko"), helper);
    assert_eq!(
        entry("lib/modules/modules.load"),
        b"# vendor list\nsensor.ko\nhelper.ko\n"
    );
    assert_eq!(
        entry("lib/modules/modules.dep"),
        b"stock.ko:\nsensor.ko: helper.ko stock.ko\nhelper.ko: stock.ko\n"
    );
    assert_eq!(patch(dir, "patched.img", "again.img", &selection), changed);
    assert_eq!(fs::read(dir.join("vendor.img")).unwrap(), image);
}

#[test]
fn vendor_v4_changes_only_the_named_normal_boot_fragment() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let raw = archive(&[
        ("lib/modules/6.6/modules.dep", b"stock.ko:\n"),
        ("lib/modules/6.6/stock.ko", &module("stock", "")),
    ]);
    let other = gzip(&archive(&[("init.rc", b"stock rc")]));
    let image = vendor(
        4,
        &[
            ("recovery", 2, &other),
            ("board_a", 3, &raw),
            ("board_b", 3, &raw),
        ],
    );
    fs::write(dir.join("vendor.img"), &image).unwrap();
    for selection in [
        vec!["--enable", "stock"],
        vec!["--enable", "stock", "--fragment", "recovery"],
    ] {
        let mut args = vec!["patch", "vendor.img", "-o", "bad.img"];
        args.extend(selection);
        assert!(!invoke(dir, &args).status.success());
        assert!(!dir.join("bad.img").exists());
    }
    let changed = patch(
        dir,
        "vendor.img",
        "patched.img",
        &["--enable", "stock", "--fragment", "board_b"],
    );
    let parsed = BootImage::parse(&changed).unwrap();
    let rd = parsed.get_blocks().get_ramdisk().unwrap();
    assert_eq!(rd.get_vendor_ramdisk(0).unwrap().get_data(), other);
    assert_eq!(rd.get_vendor_ramdisk(1).unwrap().get_data(), raw);
    let patched = rd.get_vendor_ramdisk(2).unwrap().get_data();
    assert!(patched.starts_with(&raw));
    let cpio = Cpio::load_from_data(patched).unwrap();
    assert_eq!(
        cpio.entry_by_name("lib/modules/6.6/modules.load")
            .unwrap()
            .data()
            .unwrap(),
        b"stock.ko\n"
    );
}

#[test]
fn invalid_inputs_never_publish_a_partial_image() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let raw = archive(&[]);
    let image = vendor(3, &[("", 0, &raw)]);
    fs::write(dir.join("vendor.img"), &image).unwrap();
    fs::write(dir.join("bad.ko"), b"not an ELF").unwrap();
    fs::write(dir.join("sensor.ko"), module("sensor", "helper")).unwrap();
    fs::write(dir.join("helper.ko"), module("helper", "sensor")).unwrap();
    for (args, error) in [
        (vec!["-m", "bad.ko"], "ELF"),
        (vec!["-m", "sensor.ko"], "missing dependency helper"),
        (vec!["-m", "sensor.ko", "-m", "helper.ko"], "cycle"),
        (vec!["--enable", "missing"], "not in"),
    ] {
        let mut all = vec!["patch", "vendor.img", "-o", "bad.img"];
        all.extend(args);
        let run = invoke(dir, &all);
        assert!(!run.status.success() && String::from_utf8_lossy(&run.stderr).contains(error));
        assert!(!dir.join("bad.img").exists());
    }
    assert!(
        !invoke(dir, &["patch", "vendor.img", "-o", "vendor.img"])
            .status
            .success()
    );
    assert_eq!(fs::read(dir.join("vendor.img")).unwrap(), image);
}

#[cfg(unix)]
#[test]
fn flash_validates_the_requested_partition_and_propagates_fastboot_status() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let img = dir.join("vendor.img");
    fs::write(&img, vendor(3, &[("", 0, &archive(&[]))])).unwrap();
    let fastboot = dir.join("fastboot");
    fs::write(
        &fastboot,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > trace\nexit \"$FASTBOOT_RESULT\"\n",
    )
    .unwrap();
    fs::set_permissions(&fastboot, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |file: &OsStr, partition: &str, result: &str| {
        Command::new(env!("CARGO_BIN_EXE_enensu"))
            .args(["flash", "-s", "device", "--partition", partition])
            .arg(file)
            .env("PATH", dir)
            .env("FASTBOOT_RESULT", result)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    assert!(!run(img.as_os_str(), "vendor_boot", "9").status.success());
    assert_eq!(
        fs::read_to_string(dir.join("trace")).unwrap(),
        format!(
            "-s\ndevice\nflash\nvendor_boot\n{}\n",
            img.canonicalize().unwrap().display()
        )
    );
    fs::remove_file(dir.join("trace")).unwrap();
    assert!(!run(img.as_os_str(), "boot", "0").status.success());
    assert!(!dir.join("trace").exists());
    let generic = dir.join("boot.img");
    fs::write(&generic, boot(3, b"kernel", &archive(&[]), &[])).unwrap();
    assert!(
        !run(generic.as_os_str(), "vendor_boot", "0")
            .status
            .success()
    );
    assert!(!dir.join("trace").exists());
    assert!(run(generic.as_os_str(), "boot", "0").status.success());
    assert_eq!(
        fs::read_to_string(dir.join("trace")).unwrap(),
        format!(
            "-s\ndevice\nflash\nboot\n{}\n",
            generic.canonicalize().unwrap().display()
        )
    );
}

#[test]
fn preserve_avb_on_a_noop_and_return_a_signable_payload_on_change() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let raw = archive(&[
        ("lib/modules/modules.dep", b"stock.ko:\n"),
        ("lib/modules/stock.ko", &module("stock", "")),
    ]);
    let mut signed = vendor(3, &[("", 0, &raw)]);
    let payload_size = signed.len() as u64;
    signed.extend_from_slice(b"AVB0");
    signed.resize(signed.len() + 4096, 0);
    // Metadata layout only. Cryptographic signing belongs to the device build.
    let mut footer = [0; 64];
    footer[..4].copy_from_slice(b"AVBf");
    footer[4..8].copy_from_slice(&1u32.to_be_bytes());
    footer[12..20].copy_from_slice(&payload_size.to_be_bytes());
    footer[20..28].copy_from_slice(&payload_size.to_be_bytes());
    footer[28..36].copy_from_slice(&256u64.to_be_bytes());
    signed.extend(footer);
    fs::write(dir.join("signed.img"), &signed).unwrap();
    assert_eq!(patch(dir, "signed.img", "copy.img", &[]), signed);
    let changed = patch(dir, "signed.img", "payload.img", &["--enable", "stock"]);
    assert!(!changed.windows(4).any(|w| w == b"AVBf" || w == b"AVB0"));
    assert!(BootImage::parse(&changed).is_ok());
}

#[test]
fn an_explicit_module_directory_selects_one_kernel_version() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let raw = archive(&[
        ("lib/modules/6.1/modules.load", b""),
        ("lib/modules/6.6/modules.load", b""),
    ]);
    fs::write(dir.join("vendor.img"), vendor(3, &[("", 0, &raw)])).unwrap();
    let sensor = module("sensor", "");
    fs::write(dir.join("sensor.ko"), &sensor).unwrap();
    let auto = invoke(
        dir,
        &["patch", "vendor.img", "-o", "auto.img", "-m", "sensor.ko"],
    );
    assert!(
        !auto.status.success() && String::from_utf8_lossy(&auto.stderr).contains("--module-dir")
    );
    let changed = patch(
        dir,
        "vendor.img",
        "chosen.img",
        &["-m", "sensor.ko", "--module-dir", "lib/modules/6.6"],
    );
    let image = BootImage::parse(&changed).unwrap();
    let bytes = image.get_blocks().get_ramdisk().unwrap().get_data();
    let cpio = Cpio::load_from_data(bytes).unwrap();
    assert!(!cpio.exists("lib/modules/6.1/sensor.ko"));
    assert_eq!(
        cpio.entry_by_name("lib/modules/6.6/sensor.ko")
            .unwrap()
            .data()
            .unwrap(),
        sensor
    );
}

fn boot(version: u8, kernel: &[u8], ramdisk: &[u8], signature: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 4096];
    data[..8].copy_from_slice(b"ANDROID!");
    word(&mut data, 8, kernel.len());
    word(&mut data, 12, ramdisk.len());
    word(&mut data, 20, if version == 3 { 1580 } else { 1584 });
    word(&mut data, 40, version.into());
    data[44..56].copy_from_slice(b"console=test");
    if version == 4 {
        word(&mut data, 1580, signature.len());
    }
    for block in [kernel, ramdisk, signature] {
        data.extend_from_slice(block);
        data.resize(data.len().next_multiple_of(4096), 0);
    }
    data
}

#[test]
fn boot_image_contains_the_selected_driver_and_original_first_stage_init() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let raw = archive(&[
        ("init", b"stock first stage init"),
        ("init.rc", b"stock service definitions"),
        ("lib/modules/modules.dep", b""),
        ("lib/modules/modules.load", b"# stock"),
    ]);
    let sensor = module("sensor", "");
    fs::write(dir.join("sensor.ko"), &sensor).unwrap();
    for version in [3, 4] {
        let src = format!("boot-{version}.img");
        let out = format!("out-{version}.img");
        let sig: &[u8] = if version == 4 {
            b"old GKI certificate"
        } else {
            b""
        };
        let image = boot(version, b"original kernel", &gzip(&raw), sig);
        fs::write(dir.join(&src), &image).unwrap();
        assert_eq!(patch(dir, &src, &format!("copy-{version}.img"), &[]), image);
        let changed = patch(dir, &src, &out, &["-m", "sensor.ko"]);
        let image = BootImage::parse(&changed).unwrap();
        assert_eq!(
            image.get_blocks().get_kernel().unwrap().get_data(),
            b"original kernel"
        );
        assert_eq!(&changed[44..56], b"console=test");
        if version == 4 {
            assert_eq!(image.get_header().get_signature_size(), 0);
            assert!(!changed.windows(sig.len()).any(|b| b == sig));
        }
        let mut bytes = Vec::new();
        image
            .get_blocks()
            .get_ramdisk()
            .unwrap()
            .dump(&mut bytes, false)
            .unwrap();
        assert!(bytes.starts_with(&raw));
        let result = Cpio::load_from_data(&bytes).unwrap();
        assert_eq!(
            result
                .entry_by_name("lib/modules/sensor.ko")
                .unwrap()
                .data()
                .unwrap(),
            &sensor
        );
        assert_eq!(
            result
                .entry_by_name("lib/modules/modules.load")
                .unwrap()
                .data()
                .unwrap(),
            b"# stock\nsensor.ko\n"
        );
        let added: BTreeSet<_> = result.entries().keys().map(String::as_str).collect();
        assert_eq!(
            added,
            BTreeSet::from([
                "init",
                "init.rc",
                "lib",
                "lib/modules",
                "lib/modules/modules.dep",
                "lib/modules/modules.load",
                "lib/modules/sensor.ko"
            ])
        );
    }
}

#[test]
fn boot_uses_the_factory_vendor_lists_without_writing_the_companion() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let generic = archive(&[("init", b"original init")]);
    fs::write(dir.join("boot.img"), boot(4, b"kernel", &generic, &[])).unwrap();
    let platform = archive(&[("lib/modules/modules.load", b"stock.ko\n")]);
    let dlkm = archive(&[
        ("lib/modules/modules.dep", b"stock.ko:\n"),
        ("lib/modules/stock.ko", &module("stock", "")),
    ]);
    let factory = vendor(4, &[("base", 1, &platform), ("drivers", 3, &dlkm)]);
    fs::write(dir.join("vendor.img"), &factory).unwrap();
    let sensor = module("sensor", "stock");
    fs::write(dir.join("sensor.ko"), &sensor).unwrap();
    let run = invoke(
        dir,
        &["patch", "boot.img", "-o", "bad.img", "-m", "sensor.ko"],
    );
    assert!(
        !run.status.success() && String::from_utf8_lossy(&run.stderr).contains("--vendor-boot")
    );
    assert!(!dir.join("bad.img").exists());
    let select = ["-m", "sensor.ko", "--vendor-boot", "vendor.img"];
    let changed = patch(dir, "boot.img", "out.img", &select);
    let parsed = BootImage::parse(&changed).unwrap();
    let raw = parsed.get_blocks().get_ramdisk().unwrap().get_data();
    let result = Cpio::load_from_data(raw).unwrap();
    assert_eq!(
        result
            .entry_by_name("lib/modules/sensor.ko")
            .unwrap()
            .data()
            .unwrap(),
        sensor
    );
    assert_eq!(
        result
            .entry_by_name("lib/modules/modules.load")
            .unwrap()
            .data()
            .unwrap(),
        b"stock.ko\nsensor.ko\n"
    );
    assert_eq!(
        result
            .entry_by_name("lib/modules/modules.dep")
            .unwrap()
            .data()
            .unwrap(),
        b"stock.ko:\nsensor.ko: stock.ko\n"
    );
    assert!(!result.exists("lib/modules/stock.ko"));
    assert_eq!(fs::read(dir.join("vendor.img")).unwrap(), factory);
    assert_eq!(patch(dir, "out.img", "again.img", &select), changed);
}

#[test]
fn boot_layouts_without_a_ramdisk_or_kernel_fail_before_output() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    for (input, expected) in [
        (boot(4, b"kernel", &[], &[]), "no ramdisk"),
        (boot(4, &[], &archive(&[]), &[]), "must contain a kernel"),
    ] {
        fs::write(dir.join("boot.img"), input).unwrap();
        let run = invoke(
            dir,
            &["patch", "boot.img", "-o", "bad.img", "--enable", "stock"],
        );
        assert!(!run.status.success() && String::from_utf8_lossy(&run.stderr).contains(expected));
        assert!(!dir.join("bad.img").exists());
    }
}

#[test]
fn vendor_patch_does_not_shadow_a_factory_list_in_another_fragment() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let platform = archive(&[
        ("lib/modules/modules.load", b"stock.ko\n"),
        ("lib/modules/modules.dep", b"stock.ko:\n"),
    ]);
    fs::write(
        dir.join("vendor.img"),
        vendor(4, &[("base", 1, &platform), ("drivers", 3, &archive(&[]))]),
    )
    .unwrap();
    fs::write(dir.join("sensor.ko"), module("sensor", "")).unwrap();
    let run = invoke(
        dir,
        &["patch", "vendor.img", "-o", "bad.img", "-m", "sensor.ko"],
    );
    assert!(
        !run.status.success()
            && String::from_utf8_lossy(&run.stderr).contains("another vendor fragment")
    );
    assert!(!dir.join("bad.img").exists());
}
