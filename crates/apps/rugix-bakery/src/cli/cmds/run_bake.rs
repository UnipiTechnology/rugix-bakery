//! The `bake` command.

use std::path::Path;

use reportify::ResultExt;

use crate::cli::{args, load_project};
use crate::oven::{self, LayerBakery};
use crate::BakeryResult;

/// Run the `bake` command.
pub fn run(args: &args::Args, cmd: &args::BakeCommand) -> BakeryResult<()> {
    let project = load_project(args)?;
    match cmd {
        args::BakeCommand::Rule {
            invocation,
            output,
            source_date,
        } => {
            crate::build::build_invocation(
                &project,
                invocation,
                output,
                source_epoch(source_date)?,
            )?;
        }
        args::BakeCommand::Image {
            system,
            output,
            release,
            source_date,
            params,
            mixins,
        } => {
            let param_overrides = oven::load_parameter_overrides(&params.param_files)?;
            let system_path = Path::new("build").join(system);
            let source_date_epoch = source_epoch(source_date)?;
            oven::bake_system(
                &project,
                &release.release_info(),
                system,
                &system_path,
                source_date_epoch,
                param_overrides.clone(),
                &mixins.selection(),
                false,
            )?;
            if let Some(output) = output {
                crate::build::copy_file(&system_path.join("system.img"), output)?;
            }
        }
        args::BakeCommand::Layer {
            layer,
            arch,
            source_date,
            params,
        } => {
            let param_overrides = oven::load_parameter_overrides(&params.param_files)?;
            let source_date_epoch = source_epoch(source_date)?;
            LayerBakery::new(&project, *arch, &param_overrides)
                .bake_root(layer, source_date_epoch)?;
        }
        args::BakeCommand::Bundle {
            source_date,
            system,
            output,
            release,
            params,
            mixins,
        } => {
            let param_overrides = oven::load_parameter_overrides(&params.param_files)?;
            let system_path = Path::new("build").join(system);
            let declared = oven::bake_system(
                &project,
                &release.release_info(),
                system,
                &system_path,
                source_epoch(source_date)?,
                param_overrides.clone(),
                &mixins.selection(),
                true,
            )?;

            match oven::bake_bundle(&system_path, output.as_ref(), declared.as_deref())? {
                oven::BakedBundle::Rugix(hash) => println!("{hash}"),
                oven::BakedBundle::Declared { file, sha256 } => {
                    println!("{sha256} {}", file.display())
                }
            };
        }
    }
    Ok(())
}

/// Reject dates before the Unix epoch rather than wrapping them to unsigned seconds.
fn source_epoch(date: &Option<jiff::Timestamp>) -> BakeryResult<Option<u64>> {
    date.map(|date| {
        u64::try_from(date.as_second()).whatever("source date must not precede the Unix epoch")
    })
    .transpose()
}
