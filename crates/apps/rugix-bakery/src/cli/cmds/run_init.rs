//! The `init` command.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use colored::Colorize;
use serde::Deserialize;

use reportify::{bail, ResultExt};

use rugix_common::fsutils::copy_recursive;

use crate::cli::{args, current_dir};
use crate::project::git::materialize_template_source;
use crate::{paths, BakeryResult};

/// Run the `init` command.
pub fn run(cmd: &args::InitCommand) -> BakeryResult<()> {
    let initializing = cmd.template.is_some();
    if initializing && Path::new("rugix-bakery.toml").exists() {
        bail!("Project has already been initialized.");
    }
    let cwd = current_dir()?;
    // Resolve the root of the template source and its manifest. Templates from
    // an external repository or a local directory are declared in a
    // `rugix-templates.toml` manifest at the root of the source, while the
    // bundled templates use the legacy `templates.toml` catalog.
    let (source_root, manifest_path) = if let Some(url) = &cmd.template_repo {
        let root = materialize_template_source(url, cmd.template_rev.as_deref(), &cwd, true)
            .whatever_with(|_| format!("unable to materialize template repository {url}"))?;
        let manifest = root.join("rugix-templates.toml");
        (root, manifest)
    } else if let Some(dir) = &cmd.template_dir {
        if !dir.is_dir() {
            bail!("Template directory {dir:?} does not exist.");
        }
        let manifest = dir.join("rugix-templates.toml");
        (dir.clone(), manifest)
    } else {
        let root = paths::templates_dir();
        let manifest = root.join("templates.toml");
        (root.to_path_buf(), manifest)
    };
    let external = cmd.template_repo.is_some() || cmd.template_dir.is_some();
    let templates: HashMap<String, TemplateInfo> =
        toml::from_str(&fs::read_to_string(&manifest_path).whatever_with(|_| {
            if external {
                format!(
                    "template source does not contain a `rugix-templates.toml` manifest \
                     (expected at {})",
                    manifest_path.display()
                )
            } else {
                format!(
                    "error reading templates list from {}",
                    manifest_path.display()
                )
            }
        })?)
        .whatever_with(|_| {
            format!(
                "error parsing templates list from {}",
                manifest_path.display()
            )
        })?;
    let Some(template_name) = &cmd.template else {
        rugix_cli::suspend(|| {
            eprintln!("{}\n", "Available Templates:".bold());
            let mut names = templates.keys().collect::<Vec<_>>();
            names.sort();
            for name in names {
                let info = templates.get(name).unwrap();
                eprintln!(
                    "  {}:\n    {}",
                    name.blue(),
                    info.description.trim().bright_black()
                );
            }
        });
        return Ok(());
    };
    let Some(info) = templates.get(template_name) else {
        let mut names = templates.keys().map(String::as_str).collect::<Vec<_>>();
        names.sort_unstable();
        bail!(
            "Unknown template `{template_name}`. Available templates: {}",
            names.join(", ")
        );
    };
    let template_dir = source_root.join(info.path.as_deref().unwrap_or(template_name));
    copy_recursive(template_dir, cwd).whatever("error copying template to project directory")?;
    Ok(())
}

/// Template information.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateInfo {
    /// Description of the template.
    pub description: String,
    /// Path of the template directory relative to the source root.
    ///
    /// Defaults to a directory named after the template.
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Parse a manifest and resolve a template directory.
    fn resolve(manifest: &str, name: &str) -> Option<PathBuf> {
        let templates: HashMap<String, TemplateInfo> = toml::from_str(manifest).unwrap();
        let info = templates.get(name)?;
        Some(Path::new("/source").join(info.path.as_deref().unwrap_or(name)))
    }

    /// The path of a template defaults to a directory named after it.
    #[test]
    fn template_path_defaults_to_name() {
        assert_eq!(
            resolve(
                r#"
[baseos-edge]
description = "Unipi base OS."
"#,
                "baseos-edge",
            )
            .unwrap(),
            Path::new("/source/baseos-edge")
        );
    }

    /// An explicit path overrides the default directory name.
    #[test]
    fn template_path_override() {
        assert_eq!(
            resolve(
                r#"
[baseos-edge]
description = "Unipi base OS."
path = "templates/baseos-edge"
"#,
                "baseos-edge",
            )
            .unwrap(),
            Path::new("/source/templates/baseos-edge")
        );
    }

    /// Initializing from a local directory copies the template and refuses to
    /// overwrite an existing project.
    #[test]
    fn init_from_local_template_dir() {
        let source = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        fs::create_dir_all(
            source
                .path()
                .join("test-template")
                .join("layers")
                .join("test"),
        )
        .unwrap();
        fs::write(
            source.path().join("rugix-templates.toml"),
            "[test-template]\ndescription = \"Test template.\"\n",
        )
        .unwrap();
        fs::write(
            source
                .path()
                .join("test-template")
                .join("rugix-bakery.toml"),
            "",
        )
        .unwrap();
        let cmd = args::InitCommand {
            template: Some("test-template".to_owned()),
            template_repo: None,
            template_dir: Some(source.path().to_path_buf()),
            template_rev: None,
        };
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(project.path()).unwrap();
        let initialized = run(&cmd);
        let rejected_twice = run(&cmd).is_err();
        // Restore the working directory before dropping the temporary project
        // directory.
        std::env::set_current_dir(original_dir).unwrap();
        initialized.unwrap();
        assert!(project.path().join("rugix-bakery.toml").exists());
        assert!(project.path().join("layers").join("test").exists());
        assert!(rejected_twice, "initializing twice must fail");
    }
}
