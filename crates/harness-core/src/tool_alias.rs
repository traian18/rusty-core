//! Tools that share another tool's grant without being the same tool.
//!
//! Two relationships, kept apart on purpose:
//!
//! * An **alias** follows another tool *everywhere*. `edit_file` changes part
//!   of a file where `write_file` replaces all of it, but both change files,
//!   so every rule, allow-list and permission written for `write_file` -- a
//!   read-only profile's deny rule, a "check again after writing" gate -- must
//!   cover `edit_file` too, or a model could get past it just by choosing the
//!   other tool. The reverse does not hold: a rule naming the alias matches
//!   only the alias.
//! * A **granted-with** tool is merely *admitted* wherever another tool is:
//!   by an allow-list, and by that tool's permission override. It keeps its
//!   own name for rules and history. `project_info` only reads manifests, so a
//!   profile that may `read_file` may use it; but it must not count as a
//!   `read_file` call, or a gate asking for evidence of reading would be
//!   satisfied by it.

/// `(alias, the tool it follows)`. `run_check` runs a project's own checks
/// (typecheck, test, build, ...) and `install_dependencies` installs its
/// dependencies, where `run_command` runs anything, but all of them execute
/// commands: a profile that denies `run_command` ("documentation work does not
/// run commands") must deny them too, and a gate that wants evidence of a
/// command having run is met by any of them. A rule that needs *a check* in
/// particular names `run_check`, which matches only that tool.
const ALIASES: &[(&str, &str)] = &[
    ("edit_file", "write_file"),
    ("run_check", "run_command"),
    ("install_dependencies", "run_command"),
];

/// `(tool, the tool whose grant admits it)`.
const GRANTED_WITH: &[(&str, &str)] = &[("project_info", "read_file")];

/// The tool `name` follows in rules, history, allow-lists and permissions,
/// when `name` is an alias.
pub fn followed_tool(name: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, followed)| *followed)
}

/// The tool whose grant also admits `name`: what it follows, or what it is
/// granted with. For allow-lists and permission overrides, not for rules.
pub fn granted_by(name: &str) -> Option<&'static str> {
    followed_tool(name).or_else(|| {
        GRANTED_WITH
            .iter()
            .find(|(tool, _)| *tool == name)
            .map(|(_, granting)| *granting)
    })
}

/// `tools` plus every tool a listed tool's grant also admits: scoping a
/// session to `write_file` scopes `edit_file` in as well, and `read_file`
/// brings `project_info`. Order is kept and nothing is listed twice.
pub fn with_aliases(tools: &[String]) -> Vec<String> {
    let mut all = tools.to_vec();
    for (admitted, granting) in ALIASES.iter().chain(GRANTED_WITH.iter()) {
        let wanted = tools.iter().any(|tool| tool == granting);
        if wanted && !all.iter().any(|tool| tool == admitted) {
            all.push((*admitted).to_string());
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tools: &[&str]) -> Vec<String> {
        tools.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn an_alias_follows_its_tool_and_nothing_else_does() {
        assert_eq!(followed_tool("edit_file"), Some("write_file"));
        assert_eq!(followed_tool("run_check"), Some("run_command"));
        assert_eq!(followed_tool("install_dependencies"), Some("run_command"));
        assert_eq!(followed_tool("run_command"), None);
        assert_eq!(followed_tool("write_file"), None);
        assert_eq!(followed_tool("read_file"), None);
        assert_eq!(
            followed_tool("project_info"),
            None,
            "a granted-with tool does not follow its grant in rules"
        );
    }

    #[test]
    fn a_grant_admits_both_kinds_of_tool() {
        assert_eq!(granted_by("edit_file"), Some("write_file"));
        assert_eq!(granted_by("run_check"), Some("run_command"));
        assert_eq!(granted_by("project_info"), Some("read_file"));
        assert_eq!(granted_by("read_file"), None);
        assert_eq!(granted_by("run_command"), None);
    }

    #[test]
    fn a_command_grant_scopes_run_check_in_and_nothing_else_does() {
        assert_eq!(
            with_aliases(&names(&["run_command"])),
            names(&["run_command", "run_check", "install_dependencies"])
        );
        assert_eq!(
            with_aliases(&names(&["read_file", "write_file", "run_command"])),
            names(&[
                "read_file",
                "write_file",
                "run_command",
                "edit_file",
                "run_check",
                "install_dependencies",
                "project_info"
            ])
        );
        assert_eq!(
            with_aliases(&names(&["run_check"])),
            names(&["run_check"]),
            "run_check alone does not grant run_command"
        );
    }

    #[test]
    fn scoping_to_a_tool_scopes_what_it_admits_in_once() {
        assert_eq!(
            with_aliases(&names(&["read_file", "write_file"])),
            names(&["read_file", "write_file", "edit_file", "project_info"])
        );
        assert_eq!(
            with_aliases(&names(&["write_file", "edit_file"])),
            names(&["write_file", "edit_file"]),
            "already listed"
        );
        assert_eq!(
            with_aliases(&names(&["web_search"])),
            names(&["web_search"]),
            "no grant, nothing admitted"
        );
        assert_eq!(
            with_aliases(&names(&["read_file"])),
            names(&["read_file", "project_info"]),
            "reading does not admit edit_file"
        );
        assert_eq!(
            with_aliases(&names(&["edit_file", "project_info"])),
            names(&["edit_file", "project_info"]),
            "an admitted tool alone does not grant the tool that admits it"
        );
    }
}
