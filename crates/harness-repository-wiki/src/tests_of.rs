//! Which files are tests, by convention. The rule set is small and versioned; a file is a test
//! when its path or name follows the convention of any supported language.
use std::path::Path;

pub const RULE: &str = "test-convention";
pub const RULE_VERSION: u32 = 1;

const TEST_DIRECTORIES: [&str; 6] = ["tests", "test", "__tests__", "spec", "specs", "e2e"];

/// True when `path` follows a test naming or placement convention.
pub fn is_test_file(path: &Path) -> bool {
    let in_test_directory = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| TEST_DIRECTORIES.contains(&name));
    if in_test_directory {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let stem = name.split('.').next().unwrap_or(name);
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.cpp")
        || name.ends_with("_test.c")
        || name.starts_with("test_")
            && (name.ends_with(".py") || name.ends_with(".c") || name.ends_with(".cpp"))
        || name == "conftest.py"
        || (name.ends_with(".java")
            || name.ends_with(".kt")
            || name.ends_with(".cs")
            || name.ends_with(".php"))
            && (stem.ends_with("Test") || stem.ends_with("Tests") || stem.ends_with("IT"))
}

#[cfg(test)]
mod tests {
    use super::is_test_file;
    use std::path::Path;

    #[test]
    fn follows_the_conventions_of_each_language() {
        for test in [
            "tests/api.rs",
            "src/__tests__/a.ts",
            "web/x.test.ts",
            "web/x.spec.tsx",
            "pkg/handler_test.go",
            "app/test_models.py",
            "app/models_test.py",
            "app/conftest.py",
            "src/main/OrderServiceTest.java",
            "src/test/kotlin/OrderServiceTest.kt",
            "Billing.Tests/InvoiceTests.cs",
            "app/UserTest.php",
            "native/parser_test.cpp",
        ] {
            assert!(is_test_file(Path::new(test)), "{test}");
        }
        for other in [
            "src/lib.rs",
            "web/x.ts",
            "pkg/handler.go",
            "app/models.py",
            "src/Latest.java",
            "src/main/kotlin/Latest.kt",
            "app/contest.py",
            "docs/testing.md",
        ] {
            assert!(!is_test_file(Path::new(other)), "{other}");
        }
    }
}
