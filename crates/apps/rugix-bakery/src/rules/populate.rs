//! Populate the numbered partition images of an unpopulated disk layout.
//!
//! Runtime twin of the oven pipeline's `populate` stage (see
//! `oven::pipeline::Pipeline::populate`): instead of *planning* per-partition
//! `image-ext4` / `image-fat` / `image-squashfs` rules, this internal rule
//! builds them directly through the shared filesystem builders, numbered and
//! capacity-fitted for `core/assemble-disk`. The resolved partition table is
//! read from the `image` input (the output of `core/image-layout`), so
//! capacities (explicit or tree-derived) never get a second implementation.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::images::{Filesystem, ImageLayout};
use crate::BakeryResult;
use reportify::{bail, whatever, ResultExt};
use rugix_common::disk::PartitionTable;
use sidex::types::Str;
use tempfile::TempDir;

use super::filesystems::{build_ext4, build_fat, build_squashfs, Ext4, Fat, Squashfs};
use super::layout::contained_path;
use super::source_date_epoch;

pub fn image_populate_partitions() -> BakeryResult<()> {
    let inputs = Path::new("/run/rugix/bakery/work/inputs");
    let outputs = Path::new("/run/rugix/bakery/work/outputs/partitions");
    let epoch = source_date_epoch()?;

    let layout: ImageLayout = crate::config::load_config_or_json(&inputs.join("layout"))?;
    let table = PartitionTable::read(&inputs.join("image"))
        .whatever("unable to read resolved disk geometry")?;
    fs::create_dir_all(outputs).whatever("unable to create partition outputs")?;

    let specifications: Vec<_> = layout.partitions.iter().flatten().collect();
    if specifications.len() != table.partitions.len() {
        bail!(
            "layout declares {} partitions but the disk table has {}",
            specifications.len(),
            table.partitions.len()
        );
    }

    for (specification, partition) in specifications.iter().zip(&table.partitions) {
        let Some(filesystem) = &specification.filesystem else {
            // Raw firmware regions, uninitialized, or extended container
            // partitions: nothing to populate for `core/assemble-disk`.
            continue;
        };
        let capacity = table.blocks_to_bytes(partition.size).into_raw();
        if capacity == 0 {
            bail!("partition {} has no capacity", partition.number);
        }
        let image = outputs.join(partition.number.to_string());
        let image = image
            .to_str()
            .ok_or_else(|| whatever!("partition image path is not UTF-8"))?;

        match filesystem {
            Filesystem::Ext4(options) => {
                let mkfs_options = joined_options(options.additional_options.as_deref());
                // Like the oven pipeline, the partition image is built from the raw
                // tree: `clamp-mtime` is an archive concern (see `core/archive-tree`),
                // and `build_ext4` already reproduces the image via the source epoch.
                let (tree, _staging) =
                    partition_tree(&inputs.join("roots"), specification.root.as_deref())?;
                build_ext4(
                    &tree,
                    image,
                    &Ext4 {
                        size: capacity,
                        label: String::new(),
                        uuid: String::new(),
                        block_size: 4096,
                        reserved_percent: 0,
                        mkfs_options,
                    },
                    epoch,
                )?;
            }
            Filesystem::Fat32 => {
                let (tree, _staging) =
                    partition_tree(&inputs.join("roots"), specification.root.as_deref())?;
                build_fat(
                    &tree,
                    image,
                    &Fat {
                        size: capacity,
                        minimum_size: 0,
                        free_percent: 0,
                        size_alignment: 0,
                        label: String::new(),
                        volume_id: String::new(),
                        sectors_per_cluster: 0,
                        mkfs_options: String::new(),
                    },
                    epoch,
                )?;
            }
            Filesystem::Squashfs(options) => {
                let mut additional = options
                    .additional_options
                    .as_deref()
                    .unwrap_or_default()
                    .to_vec();
                if options.no_compression.unwrap_or(false) {
                    additional.extend(["-noI", "-noD", "-noF", "-noX"].map(Str::from));
                }
                let mkfs_options = joined_options(Some(&additional));
                let (tree, _staging) =
                    partition_tree(&inputs.join("roots"), specification.root.as_deref())?;
                build_squashfs(
                    &tree,
                    image,
                    &Squashfs {
                        compression: "gzip".into(),
                        block_size: 131072,
                        mkfs_options,
                    },
                    epoch,
                )?;
            }
        }
    }
    Ok(())
}

/// Resolve a partition root to a tree.
///
/// * no root — an empty staged tree (initializes an empty filesystem, as the oven
///   pipeline does for filesystems without a root),
/// * root — the `roots` input subtree, containment-checked the same way
///   `core/image-layout` sized it.
///
/// The returned `TempDir` (when staging) must be kept alive until the
/// filesystem image has been built.
fn partition_tree(roots: &Path, root: Option<&str>) -> BakeryResult<(PathBuf, Option<TempDir>)> {
    let Some(root) = root else {
        let staging = tempfile::Builder::new()
            .prefix("populate-empty-")
            .tempdir()
            .whatever("unable to stage an empty tree")?;
        let tree = staging.path().join("tree");
        fs::create_dir_all(&tree).whatever("unable to create an empty tree")?;
        return Ok((tree, Some(staging)));
    };
    Ok((contained_path(roots, root)?, None))
}

/// Shell-join formatter arguments the way rule parameters carry them.
fn joined_options(options: Option<&[Str]>) -> String {
    shlex::try_join(
        options
            .unwrap_or_default()
            .iter()
            .map(|value| value.as_str()),
    )
    .unwrap_or_default()
}
