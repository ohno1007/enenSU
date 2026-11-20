use crate::{device::Device, image, modules};
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::{Builder, TempDir};

const PARTITIONS: [&str; 4] = ["vendor_kernel_boot", "vendor_boot", "init_boot", "boot"];

#[derive(Args)]
pub struct Options {
    /// Select a driver file. Repeat for multiple files. Omit to open the desktop file chooser.
    #[arg(short = 'm', long = "module", value_name = "FILE.ko")]
    modules: Vec<PathBuf>,
    /// Matching factory .img or a directory of extracted images. Otherwise try device readback.
    #[arg(long, value_name = "IMAGE_OR_DIR")]
    firmware: Option<PathBuf>,
    /// Choose a source partition explicitly. By default use its stock normal-boot module lists.
    #[arg(long, value_parser = PARTITIONS)]
    partition: Option<String>,
    /// Authorized ADB device serial, required when several phones are online.
    #[arg(short, long)]
    serial: Option<String>,
    /// Normal-boot driver fragment, on firmware with several board variants.
    #[arg(long)]
    fragment: Option<String>,
    /// Select the vendor_boot fragment when both vendor reference images are used.
    #[arg(long)]
    vendor_boot_fragment: Option<String>,
    /// Stock first-stage module directory, e.g. lib/modules/6.6.
    #[arg(long)]
    module_dir: Option<String>,
    /// Keep the source and prepared images in a new subdirectory here.
    #[arg(long, default_value = "enensu-images")]
    output_dir: PathBuf,
    /// Maximum seconds for each device transition or image transfer.
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout: u64,
    /// Use command-line inputs only. Report an unavailable firmware image instead of opening a dialog.
    #[arg(long)]
    no_dialog: bool,
}

impl Options {
    fn desktop(&self) -> bool {
        !self.no_dialog
            && (cfg!(any(windows, target_os = "macos"))
                || std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some())
    }

    fn pick_modules(&mut self) -> Result<()> {
        if self.modules.is_empty() {
            ensure!(self.desktop(), "select one or more .ko files with --module");
            let files = tinyfiledialogs::open_file_dialog_multi(
                "选择 Android 内核模块（可多选）",
                "",
                Some((&["*.ko"], "Kernel modules")),
            )
            .context("module selection cancelled")?;
            self.modules = files.into_iter().map(PathBuf::from).collect();
            ensure!(!self.modules.is_empty(), "no module selected");
        }
        Ok(())
    }

    fn pick_firmware(&self) -> Result<PathBuf> {
        ensure!(
            self.desktop(),
            "device readback did not provide the module image. Use --firmware with the matching factory image or extracted directory"
        );
        let path = tinyfiledialogs::open_file_dialog(
            "选择与手机当前固件匹配的启动镜像",
            "",
            Some((&["*.img"], "Android boot images")),
        )
        .context("factory image selection cancelled")?;
        Ok(PathBuf::from(path))
    }
}

#[derive(Default)]
struct Firmware {
    images: BTreeMap<String, PathBuf>,
    selected: Option<String>,
    origins: BTreeMap<String, String>,
    live: bool,
}

impl Firmware {
    fn local(input: &Path, requested: Option<&str>) -> Result<Self> {
        let input = input
            .canonicalize()
            .with_context(|| format!("open firmware {}", input.display()))?;
        let mut selected = requested.map(str::to_owned);
        let mut images = BTreeMap::new();
        let directory = if input.is_file() {
            let bytes = fs::read(&input)?;
            let parsed = image::parse_image(&bytes)?;
            let name = match requested {
                Some(name) => {
                    parsed.check_target(name, &input)?;
                    name
                }
                None => parsed.source_partition(&input)?,
            }
            .to_owned();
            images.insert(name.clone(), input.clone());
            selected = Some(name);
            input.parent().context("missing firmware directory")?
        } else {
            ensure!(
                input.is_dir(),
                "expected a factory .img or extracted firmware directory"
            );
            &input
        };
        for name in PARTITIONS {
            let f = directory.join(format!("{name}.img"));
            if f.is_file() {
                images.entry(name.to_owned()).or_insert(f);
            }
        }
        ensure!(
            !images.is_empty(),
            "no supported boot images in {}",
            directory.display()
        );
        Ok(Self {
            images,
            selected,
            ..Self::default()
        })
    }

    fn read_available(
        &mut self,
        phone: &Device,
        opts: &Options,
        dir: &Path,
        mut read: impl FnMut(&str, &Path) -> Result<Option<String>>,
    ) -> Result<()> {
        for name in PARTITIONS {
            if self.images.contains_key(name) {
                continue;
            }
            if let Some(wanted) = opts.partition.as_deref()
                && name != wanted
                && (wanted.starts_with("vendor_") || !name.starts_with("vendor_"))
            {
                continue;
            }
            let file = dir.join(format!("{name}.img"));
            let Some(physical) = read(name, &file)? else {
                if file.exists() {
                    fs::remove_file(&file)?;
                }
                continue;
            };
            let bytes = fs::read(&file)?;
            let parsed = image::parse_image(&bytes).with_context(|| format!("read back {name}"))?;
            parsed.check_target(name, &file)?;
            self.images.insert(name.to_owned(), file);
            self.origins.insert(name.to_owned(), physical);
            if opts.partition.as_deref().is_none_or(|p| p == name)
                && name.starts_with("vendor_")
                && parsed
                    .stock_module_dir(
                        opts.fragment.as_deref(),
                        opts.module_dir.as_deref(),
                        &phone.kernel,
                    )?
                    .is_some()
            {
                break;
            }
        }
        Ok(())
    }

    fn fetch(
        &mut self,
        phone: &Device,
        opts: &Options,
        dir: &Path,
        slot: Option<&str>,
    ) -> Result<()> {
        if !phone.can_fetch().unwrap_or_else(|error| {
            eprintln!("Could not query device readback support: {error}");
            false
        }) {
            return Ok(());
        }
        self.read_available(phone, opts, dir, |name, file| {
            let (physical, size) = match phone.destination(name, slot, false) {
                Ok(target) => target,
                Err(e) => {
                    eprintln!("Could not locate {name} for readback: {e}");
                    return Ok(None);
                }
            };
            if size == Some(0) {
                return Ok(None);
            }
            println!("Reading {physical} using fastboot fetch…");
            match phone.fetch(&physical, file) {
                Ok(()) => Ok(Some(physical)),
                Err(e) => {
                    eprintln!("Readback unavailable for {physical}: {e}");
                    Ok(None)
                }
            }
        })
    }

    fn path(&self, name: &str) -> Option<&Path> {
        self.images.get(name).map(PathBuf::as_path)
    }

    fn check_build(
        &self,
        phone: &Device,
        parsed: &image::ParsedImage<'_>,
        partition: &str,
        path: &Path,
    ) -> Result<()> {
        parsed.check_target(partition, path)?;
        if self.live {
            return Ok(());
        }
        let reported: Vec<_> = phone
            .props
            .iter()
            .filter(|(key, value)| {
                !value.is_empty()
                    && (key.as_str() == "ro.build.fingerprint"
                        || (key.starts_with("ro.") && key.ends_with(".build.fingerprint")))
            })
            .map(|(_, value)| value.as_str())
            .collect();
        let exact_key = if partition == "boot" {
            "ro.bootimage.build.fingerprint".to_owned()
        } else {
            format!("ro.{partition}.build.fingerprint")
        };
        match parsed.avb.fingerprints.get(partition) {
            Some(fp) if phone.props.get(&exact_key).is_some_and(|v| !v.is_empty()) => ensure!(
                phone.props[&exact_key] == *fp,
                "{partition} build fingerprint {fp} differs from the connected phone"
            ),
            Some(fp) if !reported.is_empty() => ensure!(
                reported.contains(fp),
                "{partition} build fingerprint {fp} differs from the connected phone"
            ),
            _ => eprintln!(
                "No comparable build fingerprint for {}. Using the factory image you selected.",
                path.display()
            ),
        }
        Ok(())
    }

    fn prepare(&self, phone: &Device, opts: &Options) -> Result<Option<Prepared>> {
        for name in PARTITIONS {
            if self
                .selected
                .as_deref()
                .is_some_and(|wanted| wanted != name)
            {
                continue;
            }
            let Some(path) = self.path(name) else {
                continue;
            };
            let source = fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let parsed = image::parse_image(&source)?;
            self.check_build(phone, &parsed, name, path)?;
            if !parsed.has_ramdisk() {
                continue;
            }
            let vendor_target = name.starts_with("vendor_");
            let fragment = opts.fragment.as_deref();
            let mut dir = parsed.stock_module_dir(
                if vendor_target { fragment } else { None },
                opts.module_dir.as_deref(),
                &phone.kernel,
            )?;
            if vendor_target && dir.is_none() {
                continue;
            }
            // Keep the exact references whose fingerprints were checked, even
            // if the user replaces a factory file while this job is running.
            let snapshots = TempDir::new()?;
            let mut reference_paths = BTreeMap::new();
            let mut refs = image::VendorReferences {
                boot: None,
                kernel_boot: None,
                boot_fragment: opts.vendor_boot_fragment.as_deref(),
            };
            if !vendor_target {
                refs.boot = self.path("vendor_boot");
                refs.kernel_boot = self.path("vendor_kernel_boot");
                let boot_fragment = if refs.kernel_boot.is_some() {
                    refs.boot_fragment
                } else {
                    fragment
                };
                for (part, reference, selected_fragment) in [
                    ("vendor_boot", refs.boot, boot_fragment),
                    ("vendor_kernel_boot", refs.kernel_boot, fragment),
                ] {
                    if let Some(reference) = reference {
                        let raw = fs::read(reference)?;
                        let parsed = image::parse_image(&raw)?;
                        self.check_build(phone, &parsed, part, reference)?;
                        let snapshot = snapshots.path().join(format!("{part}.img"));
                        fs::write(&snapshot, &raw)?;
                        reference_paths.insert(part, snapshot);
                        // A generic ramdisk may inherit several kernel trees. Only select
                        // a reference directory automatically when its release is exact.
                        if dir.is_none() && opts.module_dir.is_none() {
                            let exact = format!("lib/modules/{}", phone.kernel);
                            dir = parsed.stock_module_dir(
                                selected_fragment,
                                Some(&exact),
                                &phone.kernel,
                            )?;
                        }
                    }
                }
                refs.boot = reference_paths.get("vendor_boot").map(PathBuf::as_path);
                refs.kernel_boot = reference_paths
                    .get("vendor_kernel_boot")
                    .map(PathBuf::as_path);
            }
            let patched = match image::patch(
                &source,
                &opts.modules,
                &[],
                fragment,
                dir.as_deref().or(opts.module_dir.as_deref()),
                refs,
            ) {
                Ok(output) => output,
                Err(e) if e.is::<image::MissingStockLists>() => continue,
                Err(e) => return Err(e.context(format!("patch {name}"))),
            };
            return Ok(Some(Prepared {
                partition: name,
                source,
                patched,
                read_from: self.origins.get(name).cloned(),
            }));
        }
        Ok(None)
    }
}

struct Prepared {
    partition: &'static str,
    source: Vec<u8>,
    patched: Option<Vec<u8>>,
    read_from: Option<String>,
}

impl Prepared {
    fn save(&self, output_dir: &Path) -> Result<(PathBuf, PathBuf)> {
        fs::create_dir_all(output_dir)?;
        let staging = Builder::new()
            .prefix("install-")
            .tempdir_in(output_dir.canonicalize()?)?;
        let source = staging
            .path()
            .join(format!("{}-source.img", self.partition));
        let image = staging
            .path()
            .join(format!("{}-patched.img", self.partition));
        for (path, bytes) in [
            (&source, self.source.as_slice()),
            (&image, self.patched.as_deref().unwrap_or(&self.source)),
        ] {
            let mut file = fs::File::create_new(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        let _ = staging.keep();
        Ok((source, image))
    }
}

fn local_image(path: &Path, phone: &Device, opts: &Options) -> Result<Prepared> {
    Firmware::local(path, opts.partition.as_deref())?.prepare(phone, opts)?
        .context("no matching normal-boot module lists. Select the partition that owns modules.load and modules.dep, or supply an extracted firmware directory")
}

pub fn run(mut opts: Options) -> Result<()> {
    opts.pick_modules()?;
    let phone = Device::connect(opts.serial.as_deref(), Duration::from_secs(opts.timeout))?;
    let working = TempDir::new()?;
    let mut names = Vec::new();
    for (index, file) in opts.modules.iter_mut().enumerate() {
        let basename = file
            .file_name()
            .and_then(|f| f.to_str())
            .context("expected a .ko filename")?;
        let bytes = fs::read(&*file).with_context(|| format!("read {}", file.display()))?;
        names.push(
            modules::check_device(&bytes, basename, &phone.machine, &phone.kernel)
                .with_context(|| format!("check {basename}"))?,
        );
        let folder = working.path().join(index.to_string());
        fs::create_dir(&folder)?;
        let snapshot = folder.join(basename);
        fs::write(&snapshot, bytes)?;
        *file = snapshot;
    }
    names.sort();
    names.dedup();
    let mut live = Firmware {
        selected: opts.partition.clone(),
        live: true,
        ..Firmware::default()
    };
    let local = if let Some(path) = opts.firmware.as_deref() {
        Some(local_image(path, &phone, &opts)?)
    } else {
        live.read_available(&phone, &opts, working.path(), |part, file| {
            phone.pull_image(part, file)
        })?;
        live.prepare(&phone, &opts)?
    };
    phone.reboot_from_android(local.is_none())?;
    let mut return_to_android = true;
    let result = (|| -> Result<()> {
        let initial_slot = phone.verify_target()?;
        // If Android did not disclose its slot but fastboot found one, reacquire
        // any unqualified block reads with the now-confirmed slot.
        let local = if !live.images.is_empty() && phone.adb_slot() != initial_slot.as_deref() {
            live.images.clear();
            live.origins.clear();
            None
        } else {
            local
        };
        let prepared = match local {
            Some(image) => image,
            None => {
                live.fetch(&phone, &opts, working.path(), initial_slot.as_deref())?;
                match live.prepare(&phone, &opts)? {
                    Some(image) => image,
                    None => {
                        println!(
                            "Select a factory image matching this device build: {}",
                            phone
                                .props
                                .get("ro.build.fingerprint")
                                .map(String::as_str)
                                .unwrap_or("unknown")
                        );
                        local_image(&opts.pick_firmware()?, &phone, &opts)?
                    }
                }
            }
        };
        phone.enter_bootloader()?;
        let current = phone.verify_target()?;
        ensure!(
            initial_slot == current,
            "active slot changed during image preparation"
        );
        let (target, capacity) = phone.destination(prepared.partition, current.as_deref(), true)?;
        if let Some(read_from) = &prepared.read_from {
            ensure!(
                read_from == &target,
                "read back {read_from}, but the bootloader resolved the destination to {target}"
            );
        }
        let length = prepared.patched.as_ref().unwrap_or(&prepared.source).len() as u64;
        ensure!(
            capacity.is_none_or(|size| length <= size),
            "prepared image is too large for {target} ({length} bytes)"
        );
        if prepared.read_from.is_some() && prepared.patched.is_none() {
            println!("{target} already contains the selected drivers. Skipping the write.");
            phone.reboot()?;
            return_to_android = false;
            return phone.verify_boot(&names, current.as_deref());
        }
        let (source, image) = prepared.save(&opts.output_dir)?;
        println!(
            "{}: {}\nPrepared image: {}",
            if prepared.read_from.is_some() {
                "Device readback"
            } else {
                "Factory source"
            },
            source.display(),
            image.display()
        );
        println!("Flashing {target} on {}…", phone.serial);
        return_to_android = false;
        phone.flash(&target, &image).with_context(|| {
            format!(
                "write to {target} did not complete. Source image retained at {}",
                source.display()
            )
        })?;
        println!("fastboot confirmed the write to {target}.");
        phone
            .reboot()
            .context("image written. The reboot request failed")?;
        phone
            .verify_boot(&names, current.as_deref())
            .with_context(|| {
                format!(
                    "image written to {target}. Source image retained at {}",
                    source.display()
                )
            })
    })();
    if result.is_err()
        && return_to_android
        && let Err(e) = phone.reboot()
    {
        eprintln!("Could not return {} to Android: {e}", phone.serial);
    }
    result
}
