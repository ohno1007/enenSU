use android_bootimg::{
    cpio::{Cpio, CpioEntry},
    parser::BootImage,
};
use flate2::{Compression, write::GzEncoder};
use object::{Architecture, BinaryFormat, Endianness, SectionKind, write::Object};
use std::{
    collections::BTreeSet,
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

// AVB hash descriptor and footer in the AOSP on-disk format, without a signing key.
fn labeled(mut payload: Vec<u8>, partition: &str) -> Vec<u8> {
    let original = payload.len() as u64;
    let body_length = (116 + partition.len()).next_multiple_of(8);
    let mut descriptor = vec![0; 16 + body_length];
    descriptor[..8].copy_from_slice(&2u64.to_be_bytes());
    descriptor[8..16].copy_from_slice(&(body_length as u64).to_be_bytes());
    descriptor[16..24].copy_from_slice(&original.to_be_bytes());
    descriptor[56..60].copy_from_slice(&(partition.len() as u32).to_be_bytes());
    descriptor[132..132 + partition.len()].copy_from_slice(partition.as_bytes());

    let aux_size = descriptor.len().next_multiple_of(64);
    let mut metadata = vec![0; 256 + aux_size];
    metadata[..4].copy_from_slice(b"AVB0");
    metadata[4..8].copy_from_slice(&1u32.to_be_bytes());
    metadata[20..28].copy_from_slice(&(aux_size as u64).to_be_bytes());
    metadata[104..112].copy_from_slice(&(descriptor.len() as u64).to_be_bytes());
    metadata[256..256 + descriptor.len()].copy_from_slice(&descriptor);
    payload.extend_from_slice(&metadata);
    let mut footer = [0; 64];
    footer[..4].copy_from_slice(b"AVBf");
    footer[4..8].copy_from_slice(&1u32.to_be_bytes());
    footer[12..20].copy_from_slice(&original.to_be_bytes());
    footer[20..28].copy_from_slice(&original.to_be_bytes());
    footer[28..36].copy_from_slice(&(metadata.len() as u64).to_be_bytes());
    payload.extend(footer);
    payload
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
    let run = |file: &Path, partition: Option<&str>, result: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_enensu"));
        cmd.args(["flash", "-s", "device"]);
        if let Some(partition) = partition {
            cmd.args(["--partition", partition]);
        }
        cmd.arg(file)
            .env("PATH", dir)
            .env("FASTBOOT_RESULT", result)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    let trace = dir.join("trace");
    let check_trace = |file: &Path, partition: &str| {
        assert_eq!(
            fs::read_to_string(&trace).unwrap(),
            format!(
                "-s\ndevice\nflash\n{partition}\n{}\n",
                file.canonicalize().unwrap().display()
            )
        );
        fs::remove_file(&trace).unwrap();
    };
    assert!(!run(&img, Some("vendor_boot"), "9").status.success());
    check_trace(&img, "vendor_boot");
    let generic = dir.join("boot.img");
    fs::write(&generic, boot(3, b"kernel", &archive(&[]), &[])).unwrap();
    let init = dir.join("ramdisk.img");
    fs::write(
        &init,
        labeled(boot(4, &[], &archive(&[]), &[]), "init_boot"),
    )
    .unwrap();
    let drivers = dir.join("renamed.img");
    let vendor_bytes = fs::read(&img).unwrap();
    fs::write(
        &drivers,
        labeled(vendor_bytes.clone(), "vendor_kernel_boot"),
    )
    .unwrap();
    let named = dir.join("vendor_boot-final.img");
    fs::write(&named, &vendor_bytes).unwrap();
    for (file, partition) in [
        (&img, None),
        (&img, Some("boot")),
        (&generic, Some("vendor_boot")),
        (&generic, Some("init_boot")),
        (&init, Some("boot")),
        (&init, None),
        (&drivers, Some("vendor_boot")),
        (&drivers, None),
        (&named, Some("vendor_kernel_boot")),
    ] {
        assert!(!run(file, partition, "0").status.success());
        assert!(!trace.exists());
    }
    for (file, partition, expected) in [
        (&generic, Some("boot"), "boot"),
        (&init, Some("init_boot"), "init_boot"),
        (&drivers, Some("vendor_kernel_boot"), "vendor_kernel_boot"),
        (&named, None, "vendor_boot"),
        (&img, Some("vendor_kernel_boot"), "vendor_kernel_boot"),
    ] {
        assert!(run(file, partition, "0").status.success());
        check_trace(file, expected);
    }
}

#[test]
fn preserve_avb_on_a_noop_and_return_a_signable_payload_on_change() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    let raw = archive(&[
        ("lib/modules/modules.dep", b"stock.ko:\n"),
        ("lib/modules/stock.ko", &module("stock", "")),
    ]);
    let signed = labeled(vendor(3, &[("", 0, &raw)]), "vendor_boot");
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
fn init_boot_keeps_the_stock_init_and_combines_both_vendor_partition_lists() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let init = archive(&[("init", b"stock init")]);
    let base = labeled(boot(4, &[], &init, &[]), "init_boot");
    fs::write(dir.join("init_boot.img"), &base).unwrap();
    let platform = archive(&[("lib/modules/modules.load", b"stock.ko\n")]);
    let other_board = archive(&[("lib/modules/modules.load", b"other.ko\n")]);
    let vendor_boot = labeled(
        vendor(
            4,
            &[("board_a", 3, &other_board), ("board_b", 3, &platform)],
        ),
        "vendor_boot",
    );
    fs::write(dir.join("vendor_boot.img"), &vendor_boot).unwrap();
    let stock = module("stock", "");
    let drivers = archive(&[
        ("lib/modules/modules.dep", b"stock.ko:\n"),
        ("lib/modules/stock.ko", &stock),
    ]);
    let vendor_kernel = labeled(
        vendor(
            4,
            &[("different", 3, &other_board), ("drivers", 3, &drivers)],
        ),
        "vendor_kernel_boot",
    );
    fs::write(dir.join("drivers.img"), &vendor_kernel).unwrap();
    let sensor = module("sensor", "stock");
    fs::write(dir.join("sensor.ko"), &sensor).unwrap();
    let select = [
        "--module",
        "sensor.ko",
        "--vendor-boot",
        "vendor_boot.img",
        "--vendor-kernel-boot",
        "drivers.img",
        "--fragment",
        "drivers",
        "--vendor-boot-fragment",
        "board_b",
    ];

    assert_eq!(patch(dir, "init_boot.img", "copy.img", &[]), base);
    let mut incomplete = vec!["patch", "init_boot.img", "-o", "unselected.img"];
    incomplete.extend_from_slice(&select[..select.len() - 2]);
    let error = invoke(dir, &incomplete);
    assert!(
        !error.status.success()
            && String::from_utf8_lossy(&error.stderr).contains("--vendor-boot-fragment")
    );
    assert!(!dir.join("unselected.img").exists());
    let changed = patch(dir, "init_boot.img", "init-patched.img", &select);
    let image = BootImage::parse(&changed).unwrap();
    assert!(image.get_blocks().get_kernel().is_none());
    let raw = image.get_blocks().get_ramdisk().unwrap().get_data();
    assert!(raw.starts_with(&init));
    let cpio = Cpio::load_from_data(raw).unwrap();
    let bytes = |p| cpio.entry_by_name(p).unwrap().data().unwrap();
    assert_eq!(bytes("init"), b"stock init");
    assert_eq!(bytes("lib/modules/sensor.ko"), sensor);
    assert_eq!(
        bytes("lib/modules/modules.dep"),
        b"stock.ko:\nsensor.ko: stock.ko\n"
    );
    assert_eq!(bytes("lib/modules/modules.load"), b"stock.ko\nsensor.ko\n");
    assert!(!cpio.exists("lib/modules/stock.ko"));
    assert_eq!(
        patch(dir, "init-patched.img", "again.img", &select),
        changed
    );
    assert_eq!(fs::read(dir.join("vendor_boot.img")).unwrap(), vendor_boot);
    assert_eq!(fs::read(dir.join("drivers.img")).unwrap(), vendor_kernel);
    let wrong = invoke(
        dir,
        &[
            "patch",
            "init_boot.img",
            "-o",
            "bad.img",
            "--enable",
            "stock",
            "--vendor-boot",
            "drivers.img",
        ],
    );
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("labeled vendor_kernel_boot"));
    assert!(!dir.join("bad.img").exists());
}

#[test]
fn separate_vendor_driver_image_works_directly_or_alongside_a_recovery_only_reference() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let stock = module("stock", "");
    let drivers = archive(&[
        ("lib/modules/modules.load", b"# original\n"),
        ("lib/modules/modules.dep", b"stock.ko:\n"),
        ("lib/modules/stock.ko", &stock),
    ]);
    let factory = labeled(vendor(4, &[("", 0, &drivers)]), "vendor_kernel_boot");
    fs::write(dir.join("vendor_kernel_boot.img"), &factory).unwrap();
    let select = ["--enable", "stock"];
    let patched = patch(
        dir,
        "vendor_kernel_boot.img",
        "drivers-patched.img",
        &select,
    );
    let parsed = BootImage::parse(&patched).unwrap();
    let fragment = parsed
        .get_blocks()
        .get_ramdisk()
        .unwrap()
        .get_vendor_ramdisk(0)
        .unwrap();
    let tree = Cpio::load_from_data(fragment.get_data()).unwrap();
    assert_eq!(
        tree.entry_by_name("lib/modules/modules.load")
            .unwrap()
            .data()
            .unwrap(),
        b"# original\nstock.ko\n"
    );
    assert_eq!(
        tree.entry_by_name("lib/modules/stock.ko")
            .unwrap()
            .data()
            .unwrap(),
        stock
    );
    assert_eq!(
        patch(dir, "drivers-patched.img", "again.img", &select),
        patched
    );

    fs::write(
        dir.join("init_boot.img"),
        boot(4, &[], &archive(&[("init", b"init")]), &[]),
    )
    .unwrap();
    let recovery = archive(&[("lib/modules/modules.load", b"recovery.ko\n")]);
    fs::write(
        dir.join("vendor_boot.img"),
        vendor(4, &[("rescue", 2, &recovery)]),
    )
    .unwrap();
    let generic = patch(
        dir,
        "init_boot.img",
        "init-patched.img",
        &[
            "--enable",
            "stock",
            "--vendor-boot",
            "vendor_boot.img",
            "--vendor-kernel-boot",
            "vendor_kernel_boot.img",
        ],
    );
    let image = BootImage::parse(&generic).unwrap();
    let tree = Cpio::load_from_data(image.get_blocks().get_ramdisk().unwrap().get_data()).unwrap();
    assert_eq!(
        tree.entry_by_name("lib/modules/modules.load")
            .unwrap()
            .data()
            .unwrap(),
        b"# original\nstock.ko\n"
    );
    assert_eq!(
        fs::read(dir.join("vendor_kernel_boot.img")).unwrap(),
        factory
    );
    fs::write(
        dir.join("vendor_boot-conflict.img"),
        vendor(4, &[("platform", 1, &recovery)]),
    )
    .unwrap();
    let bad = invoke(
        dir,
        &[
            "patch",
            "init_boot.img",
            "-o",
            "conflict.img",
            "--enable",
            "stock",
            "--vendor-boot",
            "vendor_boot-conflict.img",
            "--vendor-kernel-boot",
            "vendor_kernel_boot.img",
        ],
    );
    assert!(
        !bad.status.success()
            && String::from_utf8_lossy(&bad.stderr).contains("conflicting vendor")
    );
    assert!(!dir.join("conflict.img").exists());
}

#[test]
fn invalid_avb_lengths_and_partition_mismatches_fail_without_output() {
    let temp = tempdir().unwrap();
    let dir = temp.path();
    let raw = vendor(4, &[("base", 1, &archive(&[]))]);
    let mut broken = labeled(raw.clone(), "vendor_boot");
    let descriptor_size = raw.len() + 256 + 8;
    broken[descriptor_size..descriptor_size + 8].copy_from_slice(&u64::MAX.to_be_bytes());
    for (image, message) in [
        (broken, "AVB descriptor"),
        (labeled(raw, "init_boot"), "AVB partition label"),
    ] {
        fs::write(dir.join("source.img"), image).unwrap();
        let error = invoke(dir, &["patch", "source.img", "-o", "bad.img"]);
        assert!(
            !error.status.success() && String::from_utf8_lossy(&error.stderr).contains(message)
        );
        assert!(!dir.join("bad.img").exists());
    }
}

#[test]
fn boot_layouts_without_a_ramdisk_or_kernel_fail_before_output() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();
    for (input, expected) in [
        (boot(4, b"kernel", &[], &[]), "no ramdisk"),
        (boot(3, &[], &archive(&[]), &[]), "expected a kernel"),
        (boot(4, &[], &[], &[]), "expected a kernel"),
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

#[cfg(unix)]
#[path = "install/mod.rs"]
mod installation;
