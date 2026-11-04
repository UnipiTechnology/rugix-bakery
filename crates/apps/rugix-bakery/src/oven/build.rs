//! Build and publish image assembly from a graph containing its layers.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use reportify::bail;

use crate::build::{self, Artifact, ExecutionContext, Executor, Planner};
use crate::config::recipes::ParameterValue;
use crate::config::rules::ArtifactKind;
use crate::config::systems::SystemConfig;
use crate::oven::system::ReleaseInfo;
use crate::project::ProjectRef;
use crate::BakeryResult;

/// Execute layers, release metadata, and image rules in one graph.
#[tracing::instrument(skip_all)]
pub fn assemble(
    project: &ProjectRef,
    mut planner: Planner,
    layer: BTreeMap<sidex::types::Str, Artifact>,
    config: &SystemConfig,
    bundle: bool,
    bundle_params: HashMap<String, ParameterValue>,
    release: &ReleaseInfo,
    system: &str,
    output: &Path,
    context: ExecutionContext,
) -> BakeryResult<Option<sidex::types::Str>> {
    let inputs = planner.release(&layer, config.architecture, release, system);
    let context_artifact = inputs["context"].clone();

    let assemble_inputs = inputs.clone();
    let bundle_default = bundle && config.bundle.is_none();

    let mut outputs = if let Some(invocation) = &config.build {
        planner.invocation(invocation, assemble_inputs)?
    } else {
        super::pipeline::plan(&mut planner, assemble_inputs, config, bundle_default)
    };
    if !outputs.contains_key("image") {
        bail!("system assembly must export an image artifact");
    }
    if outputs
        .insert("build-context.json".into(), context_artifact)
        .is_some()
    {
        bail!("assembly output build-context.json is reserved for release metadata");
    }

    if let Some(invocation) = &config.sbom {
        // A system-declared SBOM rule replaces the standard SBOM generation.
        // It receives the assembly outputs and bake inputs by name and must
        // export the canonical `sbom.cdx.json` file; its outputs are published
        // alongside the assembly outputs.
        let mut available = outputs.clone();
        for (name, artifact) in &inputs {
            available
                .entry(name.clone())
                .or_insert_with(|| artifact.clone());
        }
        let artifacts = planner.invocation(invocation, available)?;
        let Some(cdx) = artifacts.get("sbom.cdx.json") else {
            bail!("system-declared SBOM rule must export `sbom.cdx.json`");
        };
        if cdx.kind != ArtifactKind::File {
            bail!("system-declared SBOM rule must export `sbom.cdx.json` as a file");
        }
        for (name, artifact) in &artifacts {
            if matches!(
                name.as_str(),
                "image" | "bundle" | "build-context.json" | "system-build-info.json"
            ) {
                bail!("SBOM rule output {name:?} is reserved");
            }
            if outputs.insert(name.clone(), artifact.clone()).is_some() {
                bail!("SBOM rule output {name:?} collides with an assembly output");
            }
        }
    }

    let bundle_name = if bundle {
        if let Some(invocation) = &config.bundle {
            let mut invocation = invocation.clone();
            let params = invocation.parameters.get_or_insert_default();

            for (name, value) in bundle_params {
                params.insert(
                    name.into(),
                    crate::config::rules::ParameterBinding::Value(value.clone()),
                );
            }

            let mut available = outputs.clone();
            for (name, artifact) in &inputs {
                available
                    .entry(name.clone())
                    .or_insert_with(|| artifact.clone());
            }

            let artifacts = planner.invocation(&invocation, available)?;
            let Some((name, artifact)) = artifacts.first_key_value() else {
                bail!("bundle rule exported no artifacts")
            };
            if artifacts.len() != 1 {
                bail!("bundle rule must export exactly one artifact");
            }
            if artifact.kind != ArtifactKind::File {
                bail!("bundle rule must export a file artifact");
            }
            if matches!(name.as_str(), "image" | "bundle" | "build-context.json") {
                bail!("bundle rule output {name:?} is reserved");
            }
            if outputs.insert(name.clone(), artifact.clone()).is_some() {
                bail!("bundle rule output {name:?} collides with an assembly output");
            }

            Some(name.clone())
        } else {
            None
        }
    } else {
        None
    };

    let plan = planner.finish(outputs);
    let mut executor = Executor::for_project(project, context)?;
    let artifacts = executor.execute(&plan)?;
    if !artifacts["image"].is_file() {
        bail!("system assembly image must be a file");
    }
    let mut publication = BTreeMap::new();
    for (name, source) in artifacts {
        let filename = match name.as_str() {
            "image" => "system.img",
            "bundle" => "system.rugixb",
            other => other,
        };
        if publication.insert(filename.into(), source).is_some() {
            bail!("assembly outputs collide at destination {filename:?}");
        }
    }
    build::publish(&publication, output, project.dir())?;

    Ok(bundle_name)
}
