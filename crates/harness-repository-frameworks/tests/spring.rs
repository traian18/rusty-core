mod support;

use std::sync::Arc;

use harness_repository_frameworks::SpringAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const POM: &str = "<project>\n<artifactId>shop</artifactId>\n<dependencies>\n<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>\n</dependencies>\n</project>\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(SpringAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

const BASE: &str = "src/main/java/com/acme";

#[tokio::test]
async fn controller_routes_combine_class_and_method_mappings() {
    let repo = repo(
        &[
            ("pom.xml", POM),
            ("src/main/java/com/acme/web/UserController.java", "package com.acme.web;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\n@RequestMapping(\"/users\")\npublic class UserController {\n  @GetMapping\n  public String list() { return \"\"; }\n  @GetMapping(\"/{id}\")\n  public String get(@PathVariable Long id) { return \"\"; }\n  @PostMapping(value = {\"/new\", \"/create\"})\n  public void create() {}\n  @RequestMapping(path = \"/legacy\", method = RequestMethod.PUT)\n  public void legacy() {}\n  @DeleteMapping(Paths.ITEM)\n  public void remove() {}\n  public void helper() {}\n}\n\n@Controller\nclass PageController {\n  @RequestMapping(\"/\")\n  public String home() { return \"\"; }\n}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary().into_iter().filter(|s| s.starts_with("RouteRegisteredTo")).collect()),
        sorted(vec![
            "RouteRegisteredTo|GET /users|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
            "RouteRegisteredTo|GET /users/{id}|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
            "RouteRegisteredTo|POST /users/new|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
            "RouteRegisteredTo|POST /users/create|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
            "RouteRegisteredTo|PUT /users/legacy|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
            "RouteRegisteredTo|ANY /|src/main/java/com/acme/web/UserController.java->src/main/java/com/acme/web/UserController.java".into(),
        ])
    );
    let get = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /users/{id}")
        .unwrap();
    assert_eq!(get.to_symbol.as_deref(), Some("UserController.get"));
    assert_eq!(get.evidence.text, "@GetMapping(\"/{id}\")");
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed @DeleteMapping path")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn beans_are_injected_through_constructors_autowired_fields_and_lombok() {
    let repo = repo(
        &[
            ("pom.xml", POM),
            (&format!("{BASE}/service/UserService.java"), "package com.acme.service;\n\nimport org.springframework.stereotype.Service;\n\n@Service\npublic class UserService {}\n"),
            (&format!("{BASE}/repo/UserRepository.java"), "package com.acme.repo;\n\nimport org.springframework.data.jpa.repository.JpaRepository;\n\npublic interface UserRepository extends JpaRepository<User, Long> {}\n"),
            (&format!("{BASE}/util/Plain.java"), "package com.acme.util;\n\npublic class Plain {}\n"),
            (&format!("{BASE}/web/UserController.java"), "package com.acme.web;\n\nimport com.acme.service.UserService;\nimport com.acme.repo.UserRepository;\nimport com.acme.util.Plain;\nimport org.springframework.beans.factory.annotation.Autowired;\nimport org.springframework.web.bind.annotation.RestController;\n\n@RestController\npublic class UserController {\n  private final UserService service;\n  @Autowired private UserRepository repository;\n  private Plain notABean;\n  public UserController(UserService service, Plain plain) { this.service = service; }\n}\n"),
            (&format!("{BASE}/web/AuditListener.java"), "package com.acme.web;\n\nimport com.acme.service.UserService;\nimport lombok.RequiredArgsConstructor;\nimport org.springframework.stereotype.Component;\n\n@Component\n@RequiredArgsConstructor\npublic class AuditListener {\n  private final UserService users;\n  private static final String NAME = \"x\";\n}\n"),
            (&format!("{BASE}/web/Helper.java"), "package com.acme.web;\n\nimport com.acme.service.UserService;\n\npublic class Helper {\n  private UserService notBeanContext;\n  public Helper(UserService s) {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let injections = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("ProviderInjectedInto"))
            .collect(),
    );
    assert_eq!(
        injections,
        sorted(vec![
            format!("ProviderInjectedInto|UserService|{BASE}/service/UserService.java->{BASE}/web/UserController.java"),
            format!("ProviderInjectedInto|UserRepository|{BASE}/repo/UserRepository.java->{BASE}/web/UserController.java"),
            format!("ProviderInjectedInto|UserService|{BASE}/service/UserService.java->{BASE}/web/AuditListener.java"),
        ]),
        "plain classes are not beans, and neither a non-bean consumer nor a static constant injects anything"
    );
}

#[tokio::test]
async fn entities_map_to_tables_by_annotation_or_naming_convention() {
    let repo = repo(
        &[
            ("pom.xml", POM),
            (&format!("{BASE}/model/UserAccount.java"), "package com.acme.model;\n\nimport jakarta.persistence.*;\n\n@Entity\npublic class UserAccount {}\n"),
            (&format!("{BASE}/model/Order.java"), "package com.acme.model;\n\nimport jakarta.persistence.*;\n\n@Entity\n@Table(name = \"orders\")\npublic class Order {}\n"),
            (&format!("{BASE}/model/Dto.java"), "package com.acme.model;\n\npublic class Dto {}\n"),
        ],
        adapters(),
    )
    .await;
    let models: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::ModelMappedToTable)
        .map(|r| (r.label.clone(), r.certainty))
        .collect();
    assert_eq!(
        models,
        [
            ("orders".to_owned(), Certainty::Declared),
            ("user_account".to_owned(), Certainty::Convention)
        ]
    );
}

#[tokio::test]
async fn the_adapter_is_off_without_spring_dependencies() {
    let repo = repo(
        &[
            ("pom.xml", "<project><artifactId>x</artifactId><dependencies><dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId></dependency></dependencies></project>"),
            ("src/A.java", "package a;\n@RestController\n@RequestMapping(\"/x\")\npublic class A { @GetMapping(\"/y\") public void y() {} }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}
