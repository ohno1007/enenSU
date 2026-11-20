use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{Read, Seek},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Reply {
    status: ExitStatus,
    text: String,
}

impl Reply {
    fn checked(self, operation: &str) -> Result<String> {
        ensure!(
            self.status.success(),
            "{operation} failed ({}): {}",
            self.status,
            self.text
        );
        Ok(self.text)
    }
}

// Both tools can wait indefinitely for a missing device. Spool their output so a
// verbose tool cannot fill a pipe while the host is waiting for the child.
fn execute(cmd: &mut Command, limit: Duration) -> Result<Reply> {
    let mut log = tempfile::tempfile()?;
    let program = cmd.get_program().to_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .spawn()
        .with_context(|| {
            format!(
                "start {} from Android SDK Platform Tools",
                program.to_string_lossy()
            )
        })?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} timed out after {} seconds",
                program.to_string_lossy(),
                limit.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(40));
    };
    log.rewind()?;
    let mut bytes = Vec::new();
    log.take(2 << 20).read_to_end(&mut bytes)?;
    Ok(Reply {
        status,
        text: String::from_utf8_lossy(&bytes).trim().to_owned(),
    })
}

fn devices(list: &str, state: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let serial = words.next()?;
            (words.next() == Some(state)).then(|| serial.to_owned())
        })
        .collect()
}

fn properties(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line
                .strip_prefix('[')?
                .strip_suffix(']')?
                .split_once("]: [")?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn slot(value: &str) -> Result<Option<&str>> {
    match value.trim().trim_start_matches('_') {
        "" => Ok(None),
        s @ ("a" | "b") => Ok(Some(s)),
        s => bail!("unrecognized active slot {s:?}"),
    }
}

fn size(value: &str) -> Option<u64> {
    match value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => value.trim().parse().ok(),
    }
}

pub struct Device {
    pub adb_serial: String,
    pub serial: String,
    pub kernel: String,
    pub machine: String,
    pub props: BTreeMap<String, String>,
    adb_slot: Option<String>,
    adb_unlocked: bool,
    timeout: Duration,
    _lock: File,
}

impl Device {
    pub fn connect(requested: Option<&str>, timeout: Duration) -> Result<Self> {
        let short = timeout.min(Duration::from_secs(10));
        execute(Command::new("fastboot").arg("--version"), short)?.checked("fastboot --version")?;
        let list = execute(Command::new("adb").arg("devices"), short)?.checked("adb devices")?;
        let ready = devices(&list, "device");
        let selected = match requested {
            Some(s) if ready.iter().any(|id| id == s) => s,
            Some(s) => bail!("ADB device {s} is not online and authorized"),
            None => match ready.as_slice() {
                [s] => s,
                [] => bail!("no authorized Android device. Connect USB and approve USB debugging on the phone"),
                _ => bail!("multiple authorized devices {ready:?}. Choose one with --serial"),
            },
        }.to_owned();
        let shell = |args: &[&str]| {
            execute(
                Command::new("adb")
                    .args(["-s", &selected, "shell"])
                    .args(args),
                short,
            )?
            .checked("adb shell")
        };
        let props = properties(&shell(&["getprop"])?);
        let prop = |key: &str| props.get(key).map(String::as_str).unwrap_or("");
        let locked = prop("ro.boot.flash.locked");
        let state = prop("ro.boot.vbmeta.device_state");
        let verified = prop("ro.boot.verifiedbootstate");
        ensure!(
            locked != "1" && state != "locked" && !matches!(verified, "green" | "yellow"),
            "bootloader is locked. Installation stopped"
        );
        let adb_unlocked = locked == "0" || state == "unlocked" || verified == "orange";
        let adb_slot = slot(prop("ro.boot.slot_suffix"))?
            .or(slot(prop("ro.boot.slot"))?)
            .map(str::to_owned);
        // A wireless ADB transport ID can differ from the physical bootloader serial.
        let serial = ["ro.boot.serialno", "ro.serialno"]
            .into_iter()
            .map(prop)
            .find(|v| !v.is_empty() && *v != "unknown")
            .unwrap_or(&selected)
            .to_owned();
        let mut hasher = DefaultHasher::new();
        serial.hash(&mut hasher);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(
                std::env::temp_dir().join(format!("enensu-device-{:016x}.lock", hasher.finish())),
            )?;
        lock.try_lock()
            .context("another enensu installation is active on this device")?;
        let kernel = shell(&["uname", "-r"])?;
        let machine = shell(&["uname", "-m"])?;
        println!(
            "ADB {selected} · {} · {kernel} · {}",
            prop("ro.product.device"),
            prop("ro.build.fingerprint")
        );
        Ok(Self {
            adb_serial: selected,
            serial,
            kernel,
            machine,
            props,
            adb_slot,
            adb_unlocked,
            timeout,
            _lock: lock,
        })
    }

    fn fastboot(&self) -> Command {
        let mut cmd = Command::new("fastboot");
        cmd.args(["-s", &self.serial]);
        cmd
    }

    fn adb(&self) -> Command {
        let mut cmd = Command::new("adb");
        cmd.args(["-s", &self.adb_serial]);
        cmd
    }

    fn short(&self) -> Duration {
        self.timeout.min(Duration::from_secs(10))
    }

    pub fn reboot_from_android(&self, fetch: bool) -> Result<()> {
        let debuggable = self.props.get("ro.debuggable").is_some_and(|p| p == "1");
        let dynamic = [
            "ro.boot.dynamic_partitions",
            "ro.boot.dynamic_partitions_retrofit",
        ]
        .iter()
        .any(|k| self.props.get(*k).is_some_and(|v| v == "true" || v == "1"));
        let mode = if fetch && debuggable && dynamic {
            "fastboot"
        } else {
            "bootloader"
        };
        println!("Rebooting {} to {mode}…", self.adb_serial);
        execute(self.adb().args(["reboot", mode]), self.short())?.checked("adb reboot")?;
        self.wait_fastboot(false)
    }

    pub fn getvar(&self, key: &str) -> Result<Option<String>> {
        let reply = execute(self.fastboot().args(["getvar", key]), self.short())?;
        if !reply.status.success() {
            let lower = reply.text.to_ascii_lowercase();
            if lower.contains("remote:")
                && [
                    "not found",
                    "unknown",
                    "not supported",
                    "unsupported",
                    "not implemented",
                    "no such",
                    "invalid",
                    "does not exist",
                    "not exist",
                ]
                .iter()
                .any(|s| lower.contains(s))
            {
                return Ok(None);
            }
            bail!("fastboot getvar {key} failed: {}", reply.text);
        }
        let prefix = format!("{key}:");
        Ok(reply
            .text
            .lines()
            .map(|l| l.trim().trim_start_matches("(bootloader)").trim())
            .find_map(|l| l.strip_prefix(&prefix))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned))
    }

    fn wait_fastboot(&self, bootloader_only: bool) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let list = execute(Command::new("fastboot").arg("devices"), self.short())?
                .checked("fastboot devices")?;
            if devices(&list, "fastboot").contains(&self.serial)
                && (!bootloader_only
                    || self
                        .getvar("is-userspace")
                        .is_ok_and(|s| s.as_deref() != Some("yes")))
            {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "fastboot device {} did not appear. Check the USB connection and driver",
                self.serial
            );
            thread::sleep(Duration::from_millis(400));
        }
    }

    pub fn enter_bootloader(&self) -> Result<()> {
        if self.getvar("is-userspace")?.as_deref() == Some("yes") {
            execute(self.fastboot().args(["reboot", "bootloader"]), self.short())?
                .checked("fastboot reboot bootloader")?;
            self.wait_fastboot(true)?;
        }
        Ok(())
    }

    pub fn verify_target(&self) -> Result<Option<String>> {
        match self.getvar("unlocked")?.as_deref() {
            Some("yes" | "true" | "1") => (),
            None if self.adb_unlocked => (),
            Some("no" | "false" | "0") => bail!("fastboot reports a locked bootloader"),
            _ => bail!("cannot confirm the bootloader is unlocked"),
        }
        let raw = self.getvar("current-slot")?.unwrap_or_default();
        let fastboot_slot = slot(&raw)?;
        if let (Some(old), Some(current)) = (self.adb_slot.as_deref(), fastboot_slot) {
            ensure!(
                old == current,
                "active slot changed from {old} to {current}. Start again from Android"
            );
        }
        Ok(fastboot_slot
            .or(self.adb_slot.as_deref())
            .map(str::to_owned))
    }

    // Use the physical name so fastboot cannot silently apply the write to a different slot.
    pub fn destination(
        &self,
        name: &str,
        active: Option<&str>,
        writing: bool,
    ) -> Result<(String, Option<u64>)> {
        let has_slot = self.getvar(&format!("has-slot:{name}"))?;
        let target = match (has_slot.as_deref(), active) {
            (Some("no"), _) => name.to_owned(),
            (Some("yes"), None) => bail!("no known current slot for {name}"),
            (Some("yes") | None, Some(s)) => format!("{name}_{s}"),
            (None, None) => name.to_owned(),
            _ => bail!("unrecognized fastboot slot capability for {name}"),
        };
        let raw = self.getvar(&format!("partition-size:{target}"))?;
        let capacity = raw
            .map(|v| size(&v).with_context(|| format!("invalid size for {target}: {v}")))
            .transpose()?;
        ensure!(
            !writing || has_slot.is_some() || active.is_some() || capacity.is_some(),
            "fastboot could not identify partition {target} or its slot"
        );
        Ok((target, capacity))
    }

    // Use the ordinary authorized sync service only for readable block nodes.
    pub fn pull_image(&self, name: &str, out: &Path) -> Result<Option<String>> {
        if !self.adb_unlocked
            || (self.adb_slot.is_none()
                && self
                    .props
                    .get("ro.build.ab_update")
                    .is_some_and(|s| s == "true"))
        {
            return Ok(None);
        }
        let physical = self
            .adb_slot
            .as_deref()
            .map_or_else(|| name.to_owned(), |s| format!("{name}_{s}"));
        for directory in ["/dev/block/by-name", "/dev/block/bootdevice/by-name"] {
            let node = format!("{directory}/{physical}");
            let access = execute(
                self.adb().args(["shell", "test", "-r", &node]),
                self.short(),
            )?;
            if !access.status.success() {
                continue;
            }
            println!("Reading {physical} using adb pull…");
            let reply = execute(self.adb().args(["pull", &node]).arg(out), self.timeout)?;
            if reply.status.success() && out.metadata().is_ok_and(|m| m.len() > 0) {
                return Ok(Some(physical));
            }
            eprintln!("ADB readback unavailable for {node}: {}", reply.text);
            if out.exists() {
                std::fs::remove_file(out)?;
            }
        }
        Ok(None)
    }

    pub fn adb_slot(&self) -> Option<&str> {
        self.adb_slot.as_deref()
    }

    pub fn can_fetch(&self) -> Result<bool> {
        Ok(self
            .getvar("max-fetch-size")?
            .as_deref()
            .and_then(size)
            .is_some_and(|n| n > 0))
    }

    pub fn fetch(&self, target: &str, out: &Path) -> Result<()> {
        execute(
            self.fastboot().args(["fetch", target]).arg(out),
            self.timeout,
        )?
        .checked("fastboot fetch")?;
        Ok(())
    }

    pub fn flash(&self, target: &str, image: &Path) -> Result<()> {
        execute(
            self.fastboot().args(["flash", target]).arg(image),
            self.timeout,
        )?
        .checked("fastboot flash")?;
        Ok(())
    }

    pub fn reboot(&self) -> Result<()> {
        execute(self.fastboot().arg("reboot"), self.short())?.checked("fastboot reboot")?;
        Ok(())
    }

    pub fn verify_boot(&self, names: &[String], expected_slot: Option<&str>) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        println!("Waiting for Android on {}…", self.adb_serial);
        loop {
            let list = execute(Command::new("adb").arg("devices"), self.short())?
                .checked("adb devices")?;
            if devices(&list, "device").contains(&self.adb_serial)
                && let Ok(reply) = execute(self.adb().args(["shell", "getprop"]), self.short())
                && reply.status.success()
            {
                let props = properties(&reply.text);
                if props.get("sys.boot_completed").is_some_and(|v| v == "1") {
                    if let Some(serial) = ["ro.boot.serialno", "ro.serialno"]
                        .iter()
                        .filter_map(|k| props.get(*k))
                        .find(|v| !v.is_empty() && *v != "unknown")
                    {
                        ensure!(
                            serial == &self.serial,
                            "a different phone appeared on the ADB transport"
                        );
                    }
                    if let Some(expected) = expected_slot {
                        let prop = |k: &str| props.get(k).map(String::as_str).unwrap_or("");
                        if let Some(actual) =
                            slot(prop("ro.boot.slot_suffix"))?.or(slot(prop("ro.boot.slot"))?)
                        {
                            ensure!(
                                actual == expected,
                                "Android booted slot {actual}, expected {expected}"
                            );
                        }
                    }
                    break;
                }
            }
            ensure!(
                Instant::now() < deadline,
                "Android boot and module loading could not be confirmed before timeout"
            );
            thread::sleep(Duration::from_millis(500));
        }
        let list = execute(
            self.adb().args(["shell", "cat", "/proc/modules"]),
            self.short(),
        )?;
        if list.status.success() {
            let missing: Vec<_> = names
                .iter()
                .filter(|n| {
                    !list
                        .text
                        .lines()
                        .any(|line| line.split_whitespace().next() == Some(n.as_str()))
                })
                .collect();
            ensure!(
                missing.is_empty(),
                "Android booted, but these drivers are absent from /proc/modules: {missing:?}"
            );
            println!(
                "Android booted with these drivers loaded: {}",
                names.join(", ")
            );
        } else {
            for name in names {
                let path = format!("/sys/module/{name}/initstate");
                let reply = execute(self.adb().args(["shell", "cat", &path]), self.short())?;
                if !reply.status.success() || reply.text != "live" {
                    println!(
                        "Android booted. Stock ADB could not confirm {name}. Module loading is unverified."
                    );
                    return Ok(());
                }
            }
            println!(
                "Android booted with verified live drivers: {}",
                names.join(", ")
            );
        }
        Ok(())
    }
}
