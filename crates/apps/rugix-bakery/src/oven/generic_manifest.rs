//! Generic build manifest emission for BSP systems.
//!
//! [`emit_generic_manifest`] describes the assembled system (partition table,
//! slots, firmware blobs, versions) in a versioned
//! `manifest.json` written next to `system-build-info.json`. Everything is
//! derived from the in-memory assembly state — no post-hoc re-reads.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use sidex::types::Str;
use tracing::info;

use reportify::{bail, ResultExt};

use rugix_common::disk::{Partition, PartitionTable, PartitionTableType, PartitionType};
use rugix_common::utils::ascii_numbers;

use crate::config::bsp::{BspConfig, BspPartition};
use crate::config::systems::{Architecture, SystemConfig};
use crate::oven::system::{SystemInfo, SystemReleaseInfo};
use crate::BakeryResult;

#[derive(Serialize)]
struct GenericBundleTable {
    label: String,
}

#[derive(Serialize)]
struct GenericBundlePartition {
    number: u32,
    name: String,
    filesystem: String,
    #[serde(rename = "type")]
    ty: String,
    bootable: bool,
    start_sector: u64,
    size_sectors: u64,
    fill_device: bool,
    content_root: Option<String>,
    slot: Option<String>,
}

#[derive(Serialize)]
struct GenericBundleBlob {
    name: String,
    source_path: String,
    /// Byte offset from the start of the disk (SWU accepts numeric offsets).
    offset: u64,
}

#[derive(Serialize)]
struct GenericBundleManifest {
    schema_version: u32,
    system: String,
    platform: String,
    architecture: String,
    release: SystemReleaseInfo,
    partition_table: GenericBundleTable,
    partitions: Vec<GenericBundlePartition>,
    raw_blobs: Vec<GenericBundleBlob>,
}

fn parttype_to_str(ty: PartitionType) -> String {
    match ty {
        PartitionType::Mbr(x) => format!("{x:02x}"),
        PartitionType::Gpt(gpt) => gpt.to_hex_str(ascii_numbers::Case::Lower).to_string(),
    }
}

fn architecture_to_str(architecture: &Architecture) -> &'static str {
    match architecture {
        Architecture::Amd64 => "amd64",
        Architecture::Arm64 => "arm64",
        Architecture::Armv7 => "armv7",
        Architecture::Armhf => "armhf",
        Architecture::Arm => "arm",
    }
}

/// Emit the generic build manifest for an assembled BSP system.
///
/// All inputs are in-memory assembly state: the parsed BSP layout, the
/// partition table as written to the image, build info and the layer root
/// directories (the `filesystems` tars are repacks of those same roots).
pub(crate) fn emit_generic_manifest(
    config: &SystemConfig,
    system_info: &SystemInfo,
    bsp: &BspConfig,
    table: &PartitionTable,
    out: &Path,
) -> BakeryResult<()> {
    let Some(bsp_parts) = &bsp.image.layout.partitions else {
        bail!("BSP layout declares no partitions");
    };
    if bsp_parts.len() != table.partitions.len() {
        bail!(
            "partition count mismatch: {} in layout, {} in image",
            bsp_parts.len(),
            table.partitions.len()
        );
    }

    let last = bsp_parts.len() - 1;
    let mut rootfs: Option<(&BspPartition, &Partition)> = None;
    let partitions = bsp_parts
        .iter()
        .zip(table.partitions.iter())
        .enumerate()
        .map(|(i, (lp, tp))| {
            let slot = lp
                .slot
                .as_ref()
                .map(Str::as_str)
                .or_else(|| (lp.root.as_ref().map(Str::as_str) == Some("system")).then(|| "rootfs"));
            if slot.as_deref() == Some("rootfs") && rootfs.is_none() {
                rootfs = Some((lp, tp));
            }
            GenericBundlePartition {
                number: tp.number.into(),
                name: lp.name.as_str().to_owned(),
                filesystem: lp
                    .filesystem
                    .as_ref()
                    .map(|f| f.name().to_owned())
                    .unwrap_or_default(),
                ty: parttype_to_str(tp.ty),
                bootable: tp.bootable,
                start_sector: tp.start.into_raw(),
                size_sectors: tp.size.into_raw(),
                fill_device: i == last,
                content_root: lp.root.as_ref().map(|r| r.as_str().to_owned()),
                slot: slot.map(str::to_owned),
            }
        })
        .collect::<Vec<_>>();
    let Some((rootfs_layout_partition, _rootfs_table_partition)) = rootfs else {
        bail!(
            "no rootfs slot: no partition has root == \"system\" or an explicit rootfs slot"
        );
    };

    let rootfs_root = rootfs_layout_partition
        .root
        .as_ref()
        .map(Str::as_str)
        .unwrap_or("system");

    // Raw firmware blobs.
    let mut raw_blobs = Vec::new();
    let blob_prefix = format!("roots/{rootfs_root}/");
    for blob in bsp.image.layout.raw_blobs.iter().flatten() {
        let file = blob.file.as_str();
        let Some(source_path) = file.strip_prefix(blob_prefix.as_str()) else {
            bail!("raw blob `{file}` does not live under the rootfs root `{blob_prefix}`");
        };
        raw_blobs.push(GenericBundleBlob {
            name: Path::new(file)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| file.to_owned()),
            source_path: source_path.to_owned(),
            offset: blob.offset.raw,
        });
    }

    let manifest = GenericBundleManifest {
        schema_version: 1,
        system: system_info.name.clone(),
        platform: system_info
            .name
            .strip_prefix("baseos-")
            .unwrap_or(&system_info.name)
            .to_owned(),
        architecture: architecture_to_str(&config.architecture).to_owned(),
        release: system_info.release.clone(),
        partition_table: GenericBundleTable {
            label: match table.ty() {
                PartitionTableType::Gpt => "gpt".to_owned(),
                PartitionTableType::Mbr => "mbr".to_owned(),
            },
        },
        partitions,
        raw_blobs,
    };

    let bytes = serde_json::to_string_pretty(&manifest).whatever("unable to serialize manifest")?;
    std::fs::write(out.join("manifest.json"), &bytes)
        .whatever("unable to write `manifest.json`")?;
    let hash = si_crypto_hashes::HashAlgorithm::Sha256.hash::<Arc<[u8]>>(bytes.as_bytes());
    std::fs::write(out.join("manifest.sha256"), format!("{hash}"))
        .whatever("unable to write `manifest.sha256`")?;
    info!("generic manifest: {}", out.join("manifest.json").display());
    Ok(())
}

