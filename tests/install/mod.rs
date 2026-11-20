use super::{archive, boot, labeled, module, vendor};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

// Only these two mock Platform Tools are on PATH. Each case has its own serial,
// boot mode and transport log; no attached hardware participates in these tests.
const ADB: &str = r#"#!/bin/sh
echo "adb $*" >> "$FIXTURE/trace"
mode=$(/bin/cat "$FIXTURE/mode")
if [ "$1" = devices ]; then
  echo "List of devices attached"
  case "$mode" in android|rebooted) echo "usb-$PHONE device";; esac
  case "$CASE" in multiple) echo "second device";; esac
  exit 0
fi
[ "$1" = -s ] && [ "$2" = "usb-$PHONE" ] || exit 72
shift 2
case "$1 $2 $3" in
  "shell test -r") case "$CASE" in adbpull|partitionchange) [ "$4" = /dev/block/by-name/vendor_boot_a ] || exit 1;; *) exit 1;; esac;;
  "pull /dev/block/by-name/vendor_boot_a "*) /bin/cp "$FIXTURE/fw/vendor_boot.img" "$3";;
  "shell getprop ")
    locked=0; state=unlocked; verified=orange; current=_a; debug=0
    case "$CASE" in locked) locked=1; state=locked; verified=green;; readback|noop) debug=1;; esac
    case "$mode:$CASE" in rebooted:fallbackslot) current=_b;; esac
    case "$CASE" in singleslot) current="";; esac
    echo "[ro.serialno]: [$PHONE]"
    echo "[ro.boot.flash.locked]: [$locked]"
    echo "[ro.boot.vbmeta.device_state]: [$state]"
    echo "[ro.boot.verifiedbootstate]: [$verified]"
    echo "[ro.boot.slot_suffix]: [$current]"
    echo "[ro.debuggable]: [$debug]"
    echo "[ro.boot.dynamic_partitions]: [true]"
    echo "[ro.product.device]: [testboard]"
    echo "[ro.build.fingerprint]: [test/build/current]"
    echo "[sys.boot_completed]: [1]"
    ;;
  "shell uname -r") if [ "$CASE" = wrongkernel ]; then echo 5.10-other; else echo 6.6-fixture; fi;;
  "shell uname -m") if [ "$CASE" = wrongarch ]; then echo x86_64; else echo aarch64; fi;;
  "shell cat /proc/modules")
    case "$CASE" in hidden) echo "Permission denied" >&2; exit 1;; missing) echo stock;; *) echo "sensor 45056 0 - Live 0";; esac;;
  "shell cat /sys/module/sensor/initstate") echo "Permission denied" >&2; exit 1;;
  "reboot bootloader ") echo bl > "$FIXTURE/mode";;
  "reboot fastboot ") echo fbdev > "$FIXTURE/mode";;
  *) exit 73;;
esac
"#;

const FASTBOOT: &str = r#"#!/bin/sh
echo "fastboot $*" >> "$FIXTURE/trace"
mode=$(/bin/cat "$FIXTURE/mode")
case "$1" in
  --version) echo "fastboot version mock"; exit 0;;
  devices) if [ "$mode" = bl ] || [ "$mode" = fbdev ]; then echo "$PHONE fastboot"; fi; exit 0;;
esac
[ "$1" = -s ] && [ "$2" = "$PHONE" ] && [ "$mode" != android ] && [ "$mode" != rebooted ] || exit 74
shift 2
unknown() { echo "FAILED (remote: 'Variable not found')" >&2; exit 1; }
case "$1" in
  getvar)
    case "$2" in
      unlocked) if [ "$CASE" = fastlocked ]; then val=no; else val=yes; fi;;
      current-slot) case "$CASE" in slotchange) val=b;; singleslot) unknown;; *) val=a;; esac;;
      is-userspace) if [ "$mode" = fbdev ]; then val=yes; else val=no; fi;;
      max-fetch-size) case "$CASE" in readback|noop) val=0x800000;; *) unknown;; esac;;
      has-slot:*) case "$CASE" in singleslot|partitionchange) val=no;; *) val=yes;; esac;;
      partition-size:*)
        case "$2" in *vendor_kernel_boot_a|*vendor_boot_a|*init_boot_a|*vendor_boot) val=0x4000000;; *) val=0;; esac
        if [ "$CASE" = toobig ]; then val=0x1; fi;;
      *) unknown;;
    esac
    echo "(bootloader) $2: $val" >&2; echo "Finished. Total time: 0.01s" >&2;;
  fetch)
    [ "$mode" = fbdev ] && [ "$2" = vendor_boot_a ] || { echo "FAILED (remote: 'unavailable')" >&2; exit 1; }
    /bin/cp "$FIXTURE/fw/vendor_boot.img" "$3";;
  flash)
    [ "$mode" = bl ] || exit 76
    if [ "$CASE" = flashfail ]; then echo "FAILED (remote: 'write failed')" >&2; exit 1; fi
    /bin/cp "$3" "$FIXTURE/flashed.img"; echo "$2" > "$FIXTURE/target";;
  reboot) if [ "$2" = bootloader ]; then echo bl > "$FIXTURE/mode"; else echo rebooted > "$FIXTURE/mode"; fi;;
  *) exit 75;;
esac
"#;

fn script(dir: &Path, name: &str, source: &str) {
    let file = dir.join(name);
    fs::write(&file, source).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
}

fn factory(loaded: bool) -> Vec<u8> {
    let raw = archive(&[
        (
            "lib/modules/modules.load",
            if loaded { b"sensor.ko\n" } else { b"" },
        ),
        ("lib/modules/modules.dep", b"sensor.ko:\n"),
        ("lib/modules/sensor.ko", &module("sensor", "")),
    ]);
    labeled(vendor(4, &[("drivers", 3, &raw)]), "vendor_boot")
}

fn with_fingerprint(image: &[u8], partition: &str, value: &str) -> Vec<u8> {
    let mut footer: [u8; 64] = image[image.len() - 64..].try_into().unwrap();
    let offset = u64::from_be_bytes(footer[20..28].try_into().unwrap()) as usize;
    let mut header = image[offset..offset + 256].to_vec();
    let length = u64::from_be_bytes(header[104..112].try_into().unwrap()) as usize;
    let mut descriptors = image[offset + 256..offset + 256 + length].to_vec();
    let key = format!("com.android.build.{partition}.fingerprint");
    let body = (16 + key.len() + 1 + value.len() + 1).next_multiple_of(8);
    let mut property = vec![0; 16 + body];
    property[8..16].copy_from_slice(&(body as u64).to_be_bytes());
    property[16..24].copy_from_slice(&(key.len() as u64).to_be_bytes());
    property[24..32].copy_from_slice(&(value.len() as u64).to_be_bytes());
    property[32..32 + key.len()].copy_from_slice(key.as_bytes());
    let start = 33 + key.len();
    property[start..start + value.len()].copy_from_slice(value.as_bytes());
    descriptors.extend_from_slice(&property);
    header[104..112].copy_from_slice(&(descriptors.len() as u64).to_be_bytes());
    descriptors.resize(descriptors.len().next_multiple_of(64), 0);
    header[20..28].copy_from_slice(&(descriptors.len() as u64).to_be_bytes());
    footer[28..36].copy_from_slice(&((256 + descriptors.len()) as u64).to_be_bytes());
    [&image[..offset], &header, &descriptors, &footer].concat()
}

struct Harness {
    root: TempDir,
    serial: String,
}

impl Harness {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let dir = root.path();
        let serial = dir.file_name().unwrap().to_str().unwrap().to_owned();
        fs::create_dir(dir.join("fw")).unwrap();
        script(dir, "adb", ADB);
        script(dir, "fastboot", FASTBOOT);
        fs::write(dir.join("sensor.ko"), module("sensor", "")).unwrap();
        fs::write(dir.join("fw/vendor_boot.img"), factory(false)).unwrap();
        Self { root, serial }
    }

    fn run(&self, case: &str, args: &[&str]) -> Output {
        let dir = self.root.path();
        fs::write(dir.join("mode"), "android\n").unwrap();
        fs::write(dir.join("trace"), "").unwrap();
        Command::new(env!("CARGO_BIN_EXE_enensu"))
            .args([
                "install",
                "--module",
                "sensor.ko",
                "--no-dialog",
                "--timeout",
                "3",
                "--output-dir",
                "saved images",
            ])
            .args(args)
            .env("PATH", dir)
            .env("FIXTURE", dir)
            .env("PHONE", &self.serial)
            .env("CASE", case)
            .current_dir(dir)
            .output()
            .unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.path().join(name)).unwrap()
    }

    fn saved(&self, name: &str) -> Vec<u8> {
        let runs: Vec<_> = fs::read_dir(self.root.path().join("saved images"))
            .unwrap()
            .collect();
        assert_eq!(runs.len(), 1);
        fs::read(runs[0].as_ref().unwrap().path().join(name)).unwrap()
    }
}

fn okay(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn local_firmware_flashes_the_driver_partition_to_the_selected_phones_current_slot() {
    let test = Harness::new();
    let dir = test.root.path();
    let driver = labeled(
        vendor(
            4,
            &[(
                "",
                0,
                &archive(&[
                    ("lib/modules/modules.dep", b""),
                    ("lib/modules/modules.load", b""),
                ]),
            )],
        ),
        "vendor_kernel_boot",
    );
    fs::write(dir.join("fw/vendor_kernel_boot.img"), &driver).unwrap();
    let out = test.run(
        "multiple",
        &[
            "--firmware",
            "fw",
            "--serial",
            &format!("usb-{}", test.serial),
        ],
    );
    okay(&out);
    assert_eq!(test.read("target"), "vendor_kernel_boot_a\n");
    assert_eq!(test.saved("vendor_kernel_boot-source.img"), driver);
    let edited = test.saved("vendor_kernel_boot-patched.img");
    assert_eq!(fs::read(dir.join("flashed.img")).unwrap(), edited);
    assert!(
        edited
            .windows(b"sensor.ko:".len())
            .any(|w| w == b"sensor.ko:")
    );
    let trace = test.read("trace");
    assert_eq!(trace.lines().filter(|l| l.contains(" flash ")).count(), 1);
    assert!(!trace.contains(" fetch ") && !trace.contains("vbmeta.img"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("drivers loaded: sensor"));
}

#[test]
fn readback_uses_fastbootd_and_returns_to_bootloader_before_the_write() {
    let test = Harness::new();
    let out = test.run("readback", &[]);
    okay(&out);
    let trace = test.read("trace");
    for label in [
        "reboot fastboot",
        "fetch vendor_kernel_boot_a",
        "fetch vendor_boot_a",
        "reboot bootloader",
        "flash vendor_boot_a",
    ] {
        assert!(trace.contains(label), "{trace}");
    }
    assert_eq!(test.saved("vendor_boot-source.img"), factory(false));
    assert_eq!(test.read("mode"), "rebooted\n");
}

#[test]
fn reads_an_accessible_current_slot_through_ordinary_adb_before_reboot() {
    let test = Harness::new();
    let out = test.run("adbpull", &[]);
    okay(&out);
    let trace = test.read("trace");
    assert!(trace.contains("pull /dev/block/by-name/vendor_boot_a"));
    assert!(!trace.contains(" fetch ") && !trace.contains("reboot fastboot"));
    assert_eq!(test.read("target"), "vendor_boot_a\n");
    assert_eq!(test.saved("vendor_boot-source.img"), factory(false));
}

#[test]
fn checks_fingerprints_on_the_target_and_read_only_references_before_reboot() {
    let test = Harness::new();
    let fw = test.root.path().join("fw");
    fs::write(
        fw.join("init_boot.img"),
        labeled(
            boot(4, &[], &archive(&[("init", b"init")]), &[]),
            "init_boot",
        ),
    )
    .unwrap();
    fs::write(
        fw.join("vendor_boot.img"),
        with_fingerprint(&factory(false), "vendor_boot", "test/build/different"),
    )
    .unwrap();
    for target in ["fw/vendor_boot.img", "fw/init_boot.img"] {
        let out = test.run("local", &["--firmware", target]);
        assert!(
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr).contains("build fingerprint")
        );
        assert!(
            !test.read("trace").contains(" reboot ") && !test.read("trace").contains(" flash ")
        );
    }
    fs::write(
        fw.join("vendor_boot.img"),
        with_fingerprint(&factory(false), "vendor_boot", "test/build/current"),
    )
    .unwrap();
    okay(&test.run("local", &["--firmware", "fw/vendor_boot.img"]));
}

#[test]
fn unchanged_live_image_skips_the_write_but_still_verifies_the_driver() {
    let test = Harness::new();
    fs::write(test.root.path().join("fw/vendor_boot.img"), factory(true)).unwrap();
    let out = test.run("noop", &[]);
    okay(&out);
    assert!(!test.read("trace").contains(" flash "));
    assert!(String::from_utf8_lossy(&out.stdout).contains("drivers loaded: sensor"));
}

#[test]
fn refuses_mismatched_states_or_sources_without_writing_device_partitions() {
    for (case, args, expected) in [
        ("locked", vec!["--firmware", "fw"], "bootloader is locked"),
        ("multiple", vec!["--firmware", "fw"], "multiple authorized"),
        ("wrongkernel", vec!["--firmware", "fw"], "device runs"),
        ("wrongarch", vec!["--firmware", "fw"], "architecture"),
        ("fastlocked", vec!["--firmware", "fw"], "locked bootloader"),
        (
            "slotchange",
            vec!["--firmware", "fw"],
            "active slot changed",
        ),
        ("toobig", vec!["--firmware", "fw"], "too large"),
        ("nofetch", vec![], "--firmware"),
        ("partitionchange", vec![], "read back vendor_boot_a"),
    ] {
        let test = Harness::new();
        let out = test.run(case, &args);
        assert!(
            !out.status.success() && String::from_utf8_lossy(&out.stderr).contains(expected),
            "{case}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let trace = test.read("trace");
        assert!(!trace.contains(" flash "), "{trace}");
        if matches!(case, "locked" | "multiple" | "wrongkernel" | "wrongarch") {
            assert!(!trace.contains(" reboot "));
            assert_eq!(test.read("mode"), "android\n");
        } else {
            assert_eq!(test.read("mode"), "rebooted\n");
        }
    }
}

#[test]
fn distinguishes_flash_failure_unobservable_modules_missing_drivers_and_slot_fallback() {
    for (case, expected, success) in [
        (
            "flashfail",
            "write to vendor_boot_a did not complete",
            false,
        ),
        ("hidden", "Module loading is unverified", true),
        ("missing", "absent from /proc/modules", false),
        ("fallbackslot", "Android booted slot b", false),
        ("singleslot", "drivers loaded: sensor", true),
    ] {
        let test = Harness::new();
        let out = test.run(case, &["--firmware", "fw/vendor_boot.img"]);
        assert_eq!(out.status.success(), success);
        let report = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(report.contains(expected), "{report}");
        assert_eq!(test.saved("vendor_boot-source.img"), factory(false));
        if case == "flashfail" {
            assert_eq!(test.read("mode"), "bl\n");
        }
        if case == "singleslot" {
            assert_eq!(test.read("target"), "vendor_boot\n");
        }
    }
}

#[test]
fn explicit_init_boot_uses_read_only_vendor_references() {
    let test = Harness::new();
    let source = labeled(
        boot(4, &[], &archive(&[("init", b"init")]), &[]),
        "init_boot",
    );
    fs::write(test.root.path().join("fw/init_boot.img"), &source).unwrap();
    let out = test.run("local", &["--firmware", "fw/init_boot.img"]);
    okay(&out);
    assert_eq!(test.read("target"), "init_boot_a\n");
    assert_eq!(test.saved("init_boot-source.img"), source);
    assert_eq!(
        fs::read(test.root.path().join("fw/vendor_boot.img")).unwrap(),
        factory(false)
    );
}
