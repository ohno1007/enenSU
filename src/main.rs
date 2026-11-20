use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use enensu::{image, install};
use std::{fs, io::Write, path::PathBuf, process::Command};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Select drivers, patch the connected unlocked phone's image, flash its current slot and reboot.
    Install(install::Options),
    /// Prepare a generic or vendor boot image offline. With no selection, make an exact copy.
    Patch {
        image: PathBuf,
        /// Write to a new file.
        #[arg(short, long)]
        output: PathBuf,
        /// Add a compiled kernel module and enable it at boot. Repeat to select more.
        #[arg(short, long = "module", value_name = "FILE.ko")]
        modules: Vec<PathBuf>,
        /// Enable a module already indexed in the first-stage ramdisk.
        #[arg(long, value_name = "NAME")]
        enable: Vec<String>,
        /// Read stock module data from vendor_boot when patching boot or init_boot.
        #[arg(long, value_name = "IMAGE")]
        vendor_boot: Option<PathBuf>,
        /// Read the separate vendor_kernel_boot when the firmware supplies one.
        #[arg(long, value_name = "IMAGE")]
        vendor_kernel_boot: Option<PathBuf>,
        /// With both reference images, select a named fragment in vendor_boot.
        #[arg(long, value_name = "NAME", requires_all = ["vendor_boot", "vendor_kernel_boot"])]
        vendor_boot_fragment: Option<String>,
        /// Select a normal-boot vendor fragment (v4). With two references, selects vendor_kernel_boot.
        #[arg(long)]
        fragment: Option<String>,
        /// Select a module directory, e.g. lib/modules/6.1, if the image has several.
        #[arg(long)]
        module_dir: Option<String>,
    },
    /// Use Android Platform Tools to flash a prepared image to its partition.
    Flash {
        image: PathBuf,
        /// Source firmware partition. Defaults to vendor_boot only for images identified as vendor_boot.
        #[arg(long, value_parser = ["boot", "init_boot", "vendor_boot", "vendor_kernel_boot"])]
        partition: Option<String>,
        /// Fastboot device serial. Uses fastboot's default device selection if omitted.
        #[arg(short, long)]
        serial: Option<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Install(options) => install::run(options)?,
        Action::Patch {
            image,
            output,
            modules,
            enable,
            fragment,
            module_dir,
            vendor_boot,
            vendor_kernel_boot,
            vendor_boot_fragment,
        } => {
            let source = fs::read(&image).with_context(|| format!("read {}", image.display()))?;
            let patched = image::patch(
                &source,
                &modules,
                &enable,
                fragment.as_deref(),
                module_dir.as_deref(),
                image::VendorReferences {
                    boot: vendor_boot.as_deref(),
                    kernel_boot: vendor_kernel_boot.as_deref(),
                    boot_fragment: vendor_boot_fragment.as_deref(),
                },
            )?;
            let destination = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(".".as_ref());
            let mut tmp = tempfile::NamedTempFile::new_in(destination)?;
            tmp.write_all(patched.as_deref().unwrap_or(&source))?;
            tmp.persist_noclobber(&output)
                .with_context(|| format!("write new file {}", output.display()))?;
            println!(
                "{} {}",
                if patched.is_some() {
                    "Wrote unsigned image payload to"
                } else {
                    "Copied byte-for-byte to"
                },
                output.display()
            );
        }
        Action::Flash {
            image,
            serial,
            partition,
        } => {
            let file = image.canonicalize()?;
            let bytes = fs::read(&file)?;
            let parsed = image::parse_image(&bytes)?;
            let partition = parsed.flash_target(partition.as_deref(), &image)?;
            let mut fastboot = Command::new("fastboot");
            if let Some(serial) = serial {
                fastboot.args(["-s", &serial]);
            }
            let status = fastboot
                .args(["flash", partition])
                .arg(file)
                .status()
                .context("start fastboot from Android SDK Platform Tools")?;
            ensure!(
                status.success(),
                "fastboot flash {partition} failed ({status})"
            );
        }
    }
    Ok(())
}
