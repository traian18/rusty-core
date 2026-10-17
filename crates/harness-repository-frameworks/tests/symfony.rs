mod support;

use std::sync::Arc;

use harness_repository_frameworks::SymfonyAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const COMPOSER: &str = r#"{"name":"acme/app","require":{"php":">=8.2","symfony/framework-bundle":"^7.0","doctrine/orm":"^3"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(SymfonyAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn route_attributes_join_class_prefixes_and_methods() {
    let repo = repo(
        &[
            ("composer.json", COMPOSER),
            ("src/Controller/BlogController.php", "<?php\nnamespace App\\Controller;\n\nuse Symfony\\Component\\Routing\\Attribute\\Route;\n\n#[Route('/blog', name: 'blog_')]\nclass BlogController\n{\n    #[Route('/', name: 'index', methods: ['GET'])]\n    public function index() {}\n\n    #[Route(path: '/{slug}', methods: ['GET', 'HEAD'])]\n    public function show() {}\n\n    #[Route('/new')]\n    public function create() {}\n\n    #[Route(Paths::EDIT)]\n    public function edit() {}\n\n    public function helper() {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let f = "src/Controller/BlogController.php";
    assert_eq!(
        sorted(
            repo.summary()
                .into_iter()
                .filter(|s| s.starts_with("RouteRegisteredTo"))
                .collect()
        ),
        sorted(vec![
            format!("RouteRegisteredTo|GET /blog|{f}->{f}"),
            format!("RouteRegisteredTo|GET /blog/{{slug}}|{f}->{f}"),
            format!("RouteRegisteredTo|HEAD /blog/{{slug}}|{f}->{f}"),
            format!("RouteRegisteredTo|ANY /blog/new|{f}->{f}"),
        ])
    );
    let show = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /blog/{slug}")
        .unwrap();
    assert_eq!(show.to_symbol.as_deref(), Some("BlogController.show"));
    assert!(
        show.evidence.text.starts_with("Route(path: '/{slug}'")
            || show.evidence.text.starts_with("#[Route(path"),
        "{}",
        show.evidence.text
    );
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed #[Route] path")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn yaml_routes_name_controllers_by_class_and_method() {
    let repo = repo(
        &[
            ("composer.json", COMPOSER),
            ("config/routes.yaml", "# routes\nhome:\n    path: /\n    controller: App\\Controller\\DefaultController::home\n    methods: [GET]\n\nlegacy:\n    path: '/old'\n    controller: App\\Controller\\Legacy\n\nbroken:\n    path: /nowhere\n    controller: App\\Controller\\Missing::go\n"),
            ("src/Controller/DefaultController.php", "<?php\nnamespace App\\Controller;\n\nclass DefaultController { public function home() {} }\n"),
            ("src/Controller/Legacy.php", "<?php\nnamespace App\\Controller;\n\nclass Legacy { public function __invoke() {} }\n"),
        ],
        adapters(),
    )
    .await;
    let summary = sorted(repo.summary());
    assert_eq!(
        summary,
        sorted(vec![
            "RouteRegisteredTo|GET /|config/routes.yaml->src/Controller/DefaultController.php"
                .into(),
            "RouteRegisteredTo|ANY /old|config/routes.yaml->src/Controller/Legacy.php".into(),
            "RouteRegisteredTo|ANY /nowhere|config/routes.yaml->-".into(),
        ])
    );
    let home = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /")
        .unwrap();
    assert_eq!(home.to_symbol.as_deref(), Some("DefaultController.home"));
    assert_eq!(
        home.evidence.file.as_path().to_str(),
        Some("config/routes.yaml")
    );
}

#[tokio::test]
async fn entities_commands_and_autowiring_are_read() {
    let repo = repo(
        &[
            ("composer.json", COMPOSER),
            ("src/Entity/BlogPost.php", "<?php\nnamespace App\\Entity;\n\nuse Doctrine\\ORM\\Mapping as ORM;\n\n#[ORM\\Entity]\nclass BlogPost {}\n"),
            ("src/Entity/Author.php", "<?php\nnamespace App\\Entity;\n\n#[ORM\\Entity]\n#[ORM\\Table(name: 'writers')]\nclass Author {}\n"),
            ("src/Command/ImportCommand.php", "<?php\nnamespace App\\Command;\n\nuse Symfony\\Component\\Console\\Attribute\\AsCommand;\n\n#[AsCommand(name: 'app:import')]\nclass ImportCommand\n{\n    public function __construct(private Importer $importer, private LoggerInterface $logger) {}\n}\n"),
            ("src/Service/Importer.php", "<?php\nnamespace App\\Service;\n\nclass Importer {}\n"),
        ],
        adapters(),
    )
    .await;
    let mut tables: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::ModelMappedToTable)
        .map(|r| (r.label.clone(), r.certainty))
        .collect();
    tables.sort();
    assert_eq!(
        tables,
        [
            ("blog_post".to_owned(), Certainty::Convention),
            ("writers".to_owned(), Certainty::Declared)
        ]
    );
    let command = repo
        .relations()
        .iter()
        .find(|r| r.kind == RelationKind::CommandExposedAs)
        .unwrap();
    assert_eq!(command.label, "app:import");
    assert_eq!(
        sorted(
            repo.summary()
                .into_iter()
                .filter(|s| s.starts_with("ProviderInjectedInto"))
                .collect()
        ),
        ["ProviderInjectedInto|Importer|src/Service/Importer.php->src/Command/ImportCommand.php"],
        "interfaces and classes outside the repository are not followed"
    );
}

#[tokio::test]
async fn the_adapter_is_off_without_symfony() {
    let repo = repo(
        &[
            (
                "composer.json",
                r#"{"name":"x/y","require":{"laravel/framework":"^11"}}"#,
            ),
            (
                "src/A.php",
                "<?php\n#[Route('/x')]\nclass A { #[Route('/y')] public function y() {} }\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}
