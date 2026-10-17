//! CMake and compile databases read as text: `CMakeLists.txt` with a `project()` is a package
//! (its `find_package`, `FetchContent_Declare` and linked libraries are dependencies, its include
//! directories are recorded as `include:<root-relative directory>` facts), and
//! `compile_commands.json` provides include directories when no CMake file sits beside it.
//! Neither CMake nor a compiler is run, and Makefiles are not read.
use std::sync::Arc;

use harness_workspace::repository_intelligence::{
    DiscoveryDiagnostic, ManifestAdapter, ManifestInput, ManifestParse, Package,
};

pub fn manifest_adapters() -> Vec<Arc<dyn ManifestAdapter>> {
    vec![Arc::new(CMake), Arc::new(CompileCommands)]
}

/// A parsed CMake command: lower-case name and its arguments.
pub(crate) struct Command {
    pub name: String,
    pub args: Vec<String>,
}

/// Commands of a CMake script: comments are dropped, quoted arguments stay whole, and nested
/// parentheses are matched.
pub(crate) fn commands(text: &str) -> Vec<Command> {
    let chars: Vec<char> = text.chars().collect();
    let (mut index, mut found) = (0usize, Vec::new());
    while index < chars.len() {
        match chars[index] {
            '#' => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = index;
                while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_')
                {
                    index += 1;
                }
                let name: String = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .to_lowercase();
                let mut look = index;
                while look < chars.len() && chars[look].is_whitespace() {
                    look += 1;
                }
                if chars.get(look) != Some(&'(') {
                    continue;
                }
                index = look + 1;
                let (mut depth, mut args, mut current, mut quoted) =
                    (1usize, Vec::new(), String::new(), false);
                while index < chars.len() && depth > 0 {
                    let c = chars[index];
                    match c {
                        '"' => {
                            quoted = !quoted;
                            current.push(c);
                        }
                        '#' if !quoted => {
                            while index < chars.len() && chars[index] != '\n' {
                                index += 1;
                            }
                            continue;
                        }
                        '(' if !quoted => {
                            depth += 1;
                            current.push(c);
                        }
                        ')' if !quoted => {
                            depth -= 1;
                            if depth > 0 {
                                current.push(c);
                            }
                        }
                        c if c.is_whitespace() && !quoted => {
                            if !current.is_empty() {
                                args.push(std::mem::take(&mut current));
                            }
                        }
                        _ => current.push(c),
                    }
                    index += 1;
                }
                if !current.is_empty() {
                    args.push(current);
                }
                found.push(Command {
                    name,
                    args: args
                        .into_iter()
                        .map(|a| a.trim_matches('"').to_owned())
                        .collect(),
                });
            }
            _ => index += 1,
        }
    }
    found
}

struct CMake;

const KEYWORDS: [&str; 12] = [
    "PUBLIC",
    "PRIVATE",
    "INTERFACE",
    "SYSTEM",
    "BEFORE",
    "AFTER",
    "REQUIRED",
    "QUIET",
    "COMPONENTS",
    "OPTIONAL_COMPONENTS",
    "debug",
    "optimized",
];

impl ManifestAdapter for CMake {
    fn id(&self) -> &'static str {
        "cmake"
    }

    fn file_name(&self) -> &'static str {
        "CMakeLists.txt"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let directory = input.path.directory().to_path_buf();
        let commands = commands(input.text);
        // Only a CMakeLists.txt that declares a project is a package; the others belong to one above.
        let Some(project) = commands
            .iter()
            .find(|c| c.name == "project")
            .and_then(|c| c.args.first().cloned())
        else {
            return parse;
        };
        let targets: Vec<&str> = commands
            .iter()
            .filter(|c| matches!(c.name.as_str(), "add_library" | "add_executable"))
            .filter_map(|c| c.args.first().map(String::as_str))
            .collect();
        let (mut dependencies, mut includes) = (Vec::new(), Vec::new());
        for command in &commands {
            match command.name.as_str() {
                "find_package" | "fetchcontent_declare" => {
                    if let Some(name) = command.args.first().filter(|n| !n.starts_with('$')) {
                        dependencies.push(name.clone());
                    }
                }
                "target_link_libraries" => {
                    for library in command.args.iter().skip(1) {
                        let library = library.as_str();
                        if KEYWORDS.contains(&library)
                            || library.starts_with(['$', '-'])
                            || targets.contains(&library)
                            || library.contains('/')
                        {
                            continue;
                        }
                        dependencies.push(library.split("::").next().unwrap_or(library).to_owned());
                    }
                }
                "include_directories" => collect_directories(
                    &command.args,
                    &directory,
                    &mut includes,
                    &mut parse.diagnostics,
                    input,
                ),
                "target_include_directories" => collect_directories(
                    &command.args[command.args.len().min(1)..],
                    &directory,
                    &mut includes,
                    &mut parse.diagnostics,
                    input,
                ),
                _ => {}
            }
        }
        dependencies.sort();
        dependencies.dedup();
        includes.sort();
        includes.dedup();
        let mut package = Package::declared(project, input.path.clone(), dependencies);
        package.facts.inferred_facts = includes
            .into_iter()
            .map(|d| format!("include:{d}"))
            .collect();
        parse.packages.push(package);
        parse
    }
}

fn collect_directories(
    args: &[String],
    base: &std::path::Path,
    out: &mut Vec<String>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
    input: &ManifestInput<'_>,
) {
    for arg in args {
        if KEYWORDS.contains(&arg.as_str()) || arg.starts_with('-') {
            continue;
        }
        let mut path = arg.clone();
        for variable in [
            "${CMAKE_CURRENT_SOURCE_DIR}",
            "${PROJECT_SOURCE_DIR}",
            "${CMAKE_CURRENT_LIST_DIR}",
        ] {
            path = path.replace(variable, ".");
        }
        path = path.replace("${CMAKE_SOURCE_DIR}", "<root>");
        if path.contains("${") || path.contains("$<") {
            diagnostics.push(DiscoveryDiagnostic::for_path(input.path, format!("include directory `{arg}` uses a variable that is not evaluated; it is not used")));
            continue;
        }
        let joined = if let Some(rest) = path.strip_prefix("<root>") {
            std::path::PathBuf::from(rest.trim_start_matches('/'))
        } else {
            base.join(&path)
        };
        if let Some(normal) = normalize(&joined) {
            out.push(normal.to_string_lossy().into_owned());
        }
    }
}

pub(crate) fn normalize(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(p) => parts.push(p.to_owned()),
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.iter().collect())
}

struct CompileCommands;

impl ManifestAdapter for CompileCommands {
    fn id(&self) -> &'static str {
        "compile-commands"
    }

    fn file_name(&self) -> &'static str {
        "compile_commands.json"
    }

    fn parse(&self, input: &ManifestInput<'_>) -> ManifestParse {
        let mut parse = ManifestParse::default();
        let directory = input.root.join(input.path.directory());
        if directory.join("CMakeLists.txt").exists() {
            return parse; // the CMake file is the project
        }
        let entries: Vec<serde_json::Value> = match serde_json::from_str(input.text) {
            Ok(entries) => entries,
            Err(error) => {
                parse.diagnostics.push(DiscoveryDiagnostic::for_path(
                    input.path,
                    format!("compile_commands.json is not a valid compile database: {error}"),
                ));
                return parse;
            }
        };
        let mut includes = Vec::new();
        for entry in &entries {
            let working = entry
                .get("directory")
                .and_then(|d| d.as_str())
                .unwrap_or("");
            let arguments: Vec<String> = match (
                entry.get("arguments").and_then(|a| a.as_array()),
                entry.get("command").and_then(|c| c.as_str()),
            ) {
                (Some(list), _) => list
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect(),
                (None, Some(command)) => command.split_whitespace().map(str::to_owned).collect(),
                _ => continue,
            };
            let mut iterator = arguments.iter();
            while let Some(argument) = iterator.next() {
                let value = if let Some(rest) = argument
                    .strip_prefix("-I")
                    .or_else(|| argument.strip_prefix("-iquote"))
                {
                    if rest.is_empty() {
                        iterator.next().cloned()
                    } else {
                        Some(rest.to_owned())
                    }
                } else {
                    None
                };
                let Some(value) = value else { continue };
                let absolute = std::path::Path::new(working).join(&value);
                if let Ok(relative) = absolute.strip_prefix(input.root) {
                    if let Some(normal) = normalize(relative) {
                        includes.push(normal.to_string_lossy().into_owned());
                    }
                }
            }
        }
        includes.sort();
        includes.dedup();
        let name = input
            .path
            .directory()
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "native".into());
        let mut package = Package::declared(name, input.path.clone(), Vec::new());
        package.facts.inferred_facts = includes
            .into_iter()
            .map(|d| format!("include:{d}"))
            .collect();
        parse.packages.push(package);
        parse
    }
}
