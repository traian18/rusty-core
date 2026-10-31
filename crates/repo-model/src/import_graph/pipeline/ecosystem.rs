//! Which package ecosystem a file belongs to, from the manifests around it.

use std::collections::HashMap;

use crate::import_graph::model::LanguageProfile;
use crate::repository_intelligence::{PackageId, RepositoryStructure, RootRelativePath};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum Ecosystem {
    Cargo,
    Npm,
    Python,
    Go,
    Java,
    DotNet,
    Composer,
    Native,
}

pub(super) fn ecosystem(profile: LanguageProfile) -> Ecosystem {
    match profile {
        LanguageProfile::Rust => Ecosystem::Cargo,
        LanguageProfile::TypeScript | LanguageProfile::JavaScript => Ecosystem::Npm,
        LanguageProfile::Python => Ecosystem::Python,
        LanguageProfile::Go => Ecosystem::Go,
        LanguageProfile::Java | LanguageProfile::Kotlin => Ecosystem::Java,
        LanguageProfile::CSharp => Ecosystem::DotNet,
        LanguageProfile::Php => Ecosystem::Composer,
        LanguageProfile::C | LanguageProfile::Cpp => Ecosystem::Native,
    }
}

pub(super) fn package_directories(
    structure: &RepositoryStructure,
) -> HashMap<(Ecosystem, std::path::PathBuf), PackageId> {
    structure
        .packages
        .iter()
        .filter_map(|package| {
            let ecosystem = manifest_ecosystem(package.manifest.as_path().file_name()?.to_str()?)?;
            let directory = package.manifest.directory().to_path_buf();
            Some(((ecosystem, directory), package.id.clone()))
        })
        .collect()
}

/// The ecosystem a manifest file belongs to, by its file name; `None` for files that name no package.
pub(super) fn manifest_ecosystem(name: &str) -> Option<Ecosystem> {
    Some(match name {
        "Cargo.toml" => Ecosystem::Cargo,
        "package.json" => Ecosystem::Npm,
        "pyproject.toml" | "setup.py" | "setup.cfg" | "Pipfile" => Ecosystem::Python,
        _ if name.starts_with("requirements") && name.ends_with(".txt") => Ecosystem::Python,
        "go.mod" => Ecosystem::Go,
        "pom.xml" | "build.gradle" | "build.gradle.kts" => Ecosystem::Java,
        _ if name.ends_with(".csproj") => Ecosystem::DotNet,
        "composer.json" => Ecosystem::Composer,
        "CMakeLists.txt" | "Makefile" | "compile_commands.json" => Ecosystem::Native,
        _ => return None,
    })
}

/// The package whose manifest is the nearest ancestor of `path` in the matching ecosystem.
pub(super) fn owning_package(
    directories: &HashMap<(Ecosystem, std::path::PathBuf), PackageId>,
    ecosystem: Ecosystem,
    path: &RootRelativePath,
) -> Option<PackageId> {
    let mut directory = path.as_path().parent();
    while let Some(current) = directory {
        if let Some(id) = directories.get(&(ecosystem, current.to_path_buf())) {
            return Some(id.clone());
        }
        directory = current.parent();
    }
    None
}
