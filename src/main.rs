mod archive;
mod image;
mod modules;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{fs, io::Write, path::PathBuf, process::Command};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Prepare a boot or vendor_boot image offline. With no selection, make an exact copy.
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
        /// Read the stock module tree from the matching vendor_boot when patching boot.
        #[arg(long, value_name = "IMAGE")]
        vendor_boot: Option<PathBuf>,
        /// Select a named normal-boot vendor ramdisk fragment (v4).
        #[arg(long)]
        fragment: Option<String>,
        /// Select a module directory, e.g. lib/modules/6.1, if the image has several.
        #[arg(long)]
        module_dir: Option<String>,
    },
    /// Use Android Platform Tools to flash a prepared image to its partition.
    Flash {
        image: PathBuf,
        /// Target partition. Explicitly select boot when flashing a boot image.
        #[arg(long, value_parser = ["boot", "vendor_boot"], default_value = "vendor_boot")]
        partition: String,
        /// Fastboot device serial. Uses fastboot's default device selection if omitted.
        #[arg(short, long)]
        serial: Option<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Patch {
            image,
            output,
            modules,
            enable,
            fragment,
            module_dir,
            vendor_boot,
        } => {
            let source = fs::read(&image).with_context(|| format!("read {}", image.display()))?;
            let patched = image::patch(
                &source,
                &modules,
                &enable,
                fragment.as_deref(),
                module_dir.as_deref(),
                vendor_boot.as_deref(),
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
            let image = image.canonicalize()?;
            let bytes = fs::read(&image)?;
            let detected = image::partition(&image::parse_image(&bytes)?);
            ensure!(
                detected == partition,
                "image is for {detected}; select --partition {detected}"
            );
            let mut fastboot = Command::new("fastboot");
            if let Some(serial) = serial {
                fastboot.args(["-s", &serial]);
            }
            let status = fastboot
                .args(["flash", &partition])
                .arg(image)
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
