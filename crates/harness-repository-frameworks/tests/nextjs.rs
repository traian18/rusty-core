mod support;

use std::sync::Arc;

use harness_repository_frameworks::NextjsAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter};
use support::*;

const PACKAGE: &str = r#"{"name":"web","dependencies":{"next":"14","react":"18"}}"#;

fn assert_same_set<const N: usize>(mut actual: Vec<String>, expected: [&str; N]) {
    let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(NextjsAdapter)]
}

#[tokio::test]
async fn app_router_pages_and_route_handlers_follow_the_directory_convention() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("app/page.tsx", "export default function Home() { return null }\n"),
            ("app/about/page.tsx", "export default function About() { return null }\n"),
            ("app/(shop)/cart/page.tsx", "export default function Cart() { return null }\n"),
            ("app/blog/[slug]/page.tsx", "export default function Post() { return null }\n"),
            ("app/docs/[...path]/page.tsx", "export default function Docs() { return null }\n"),
            ("app/api/users/[id]/route.ts", "export async function GET() {}\nexport const POST = async () => {};\nexport function helper() {}\n"),
            ("app/api/health/route.ts", "async function ping() {}\nexport { ping as GET };\n"),
            ("app/layout.tsx", "export default function Layout() { return null }\n"),
            ("app/components/Button.tsx", "export function Button() { return null }\n"),
        ],
        adapters(),
    )
    .await;
    assert_same_set(
        repo.summary(),
        [
            "RouteRegisteredTo|GET /api/health|app/api/health/route.ts->app/api/health/route.ts",
            "RouteRegisteredTo|GET /api/users/:id|app/api/users/[id]/route.ts->app/api/users/[id]/route.ts",
            "RouteRegisteredTo|PAGE /|app/page.tsx->app/page.tsx",
            "RouteRegisteredTo|PAGE /about|app/about/page.tsx->app/about/page.tsx",
            "RouteRegisteredTo|PAGE /blog/:slug|app/blog/[slug]/page.tsx->app/blog/[slug]/page.tsx",
            "RouteRegisteredTo|PAGE /cart|app/(shop)/cart/page.tsx->app/(shop)/cart/page.tsx",
            "RouteRegisteredTo|PAGE /docs/*path|app/docs/[...path]/page.tsx->app/docs/[...path]/page.tsx",
            "RouteRegisteredTo|POST /api/users/:id|app/api/users/[id]/route.ts->app/api/users/[id]/route.ts",
        ],
    );
    assert!(repo
        .relations()
        .iter()
        .all(|r| r.certainty == Certainty::Convention));
    let get = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /api/users/:id")
        .unwrap();
    assert!(get.evidence.text.starts_with("export async function GET"));
    assert_eq!(get.rule.id, "nextjs-app-route");
}

#[tokio::test]
async fn pages_router_files_and_api_routes_are_read_under_src() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            (
                "src/pages/index.tsx",
                "export default function Home() { return null }\n",
            ),
            (
                "src/pages/users/[id].tsx",
                "export default function User() { return null }\n",
            ),
            (
                "src/pages/users/index.tsx",
                "export default function Users() { return null }\n",
            ),
            (
                "src/pages/api/login.ts",
                "export default function handler() {}\n",
            ),
            (
                "src/pages/_app.tsx",
                "export default function App() { return null }\n",
            ),
            (
                "src/pages/_document.tsx",
                "export default function Doc() { return null }\n",
            ),
            ("src/pages/users/User.test.tsx", "test('x', () => {})\n"),
        ],
        adapters(),
    )
    .await;
    assert_same_set(
        repo.summary(),
        [
            "RouteRegisteredTo|API /api/login|src/pages/api/login.ts->src/pages/api/login.ts",
            "RouteRegisteredTo|PAGE /|src/pages/index.tsx->src/pages/index.tsx",
            "RouteRegisteredTo|PAGE /users|src/pages/users/index.tsx->src/pages/users/index.tsx",
            "RouteRegisteredTo|PAGE /users/:id|src/pages/users/[id].tsx->src/pages/users/[id].tsx",
        ],
    );
}

#[tokio::test]
async fn unsupported_route_forms_and_empty_handlers_are_reported() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            (
                "app/@modal/login/page.tsx",
                "export default function Modal() { return null }\n",
            ),
            (
                "app/feed/(.)photo/page.tsx",
                "export default function P() { return null }\n",
            ),
            ("app/api/none/route.ts", "export function helper() {}\n"),
            (
                "app/optional/[[...slug]]/page.tsx",
                "export default function O() { return null }\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert_eq!(repo.summary(), ["RouteRegisteredTo|PAGE /optional/*slug?|app/optional/[[...slug]]/page.tsx->app/optional/[[...slug]]/page.tsx"]);
    let diagnostics = repo.diagnostics();
    assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
    assert!(diagnostics.iter().any(|d| d.contains("@modal")));
    assert!(diagnostics.iter().any(|d| d.contains("(.)photo")));
    assert!(diagnostics.iter().any(|d| d.contains("exports no GET")));
}

#[tokio::test]
async fn the_adapter_is_off_without_the_next_dependency_and_ignores_other_directories() {
    let off = repo(
        &[
            (
                "package.json",
                r#"{"name":"w","dependencies":{"react":"18"}}"#,
            ),
            (
                "app/page.tsx",
                "export default function Home() { return null }\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(off.relations().is_empty());
    assert!(off.snapshot.frameworks.applications.is_empty());

    let elsewhere = repo(
        &[
            ("package.json", PACKAGE),
            (
                "lib/pages/page.tsx",
                "export default function X() { return null }\n",
            ),
            (
                "components/app/page.tsx",
                "export default function Y() { return null }\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(
        elsewhere.relations().is_empty(),
        "{:?}",
        elsewhere.summary()
    );
}
