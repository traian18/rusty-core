mod support;

use std::sync::Arc;

use harness_repository_frameworks::LaravelAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const COMPOSER: &str = r#"{"name":"acme/shop","require":{"php":"^8.2","laravel/framework":"^11"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(LaravelAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn routes_resolve_controllers_prefixes_closures_and_resources() {
    let repo = repo(
        &[
            ("composer.json", COMPOSER),
            ("routes/web.php", "<?php\n\nuse App\\Http\\Controllers\\UserController;\nuse App\\Http\\Controllers\\PhotoController;\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/', function () { return view('welcome'); });\nRoute::get('/users', [UserController::class, 'index'])->name('users');\nRoute::post('/legacy', 'UserController@store');\nRoute::match(['get', 'post'], '/both', [UserController::class, 'both']);\nRoute::resource('photos', PhotoController::class);\nRoute::prefix('admin')->group(function () {\n    Route::delete('/users/{id}', [UserController::class, 'destroy']);\n    Route::prefix('v2')->group(function () {\n        Route::get('/stats', [UserController::class, 'stats']);\n    });\n});\n$path = '/dyn';\nRoute::get($path, [UserController::class, 'index']);\nNotRoute::get('/x', 'y');\n"),
            ("app/Http/Controllers/UserController.php", "<?php\nnamespace App\\Http\\Controllers;\n\nclass UserController {}\n"),
            ("app/Http/Controllers/PhotoController.php", "<?php\nnamespace App\\Http\\Controllers;\n\nclass PhotoController {}\n"),
        ],
        adapters(),
    )
    .await;
    let u = "app/Http/Controllers/UserController.php";
    assert_eq!(
        sorted(repo.summary().into_iter().filter(|s| s.starts_with("RouteRegisteredTo")).collect()),
        sorted(vec![
            "RouteRegisteredTo|GET /|routes/web.php->routes/web.php".into(),
            format!("RouteRegisteredTo|GET /users|routes/web.php->{u}"),
            format!("RouteRegisteredTo|POST /legacy|routes/web.php->{u}"),
            format!("RouteRegisteredTo|GET /both|routes/web.php->{u}"),
            format!("RouteRegisteredTo|POST /both|routes/web.php->{u}"),
            "RouteRegisteredTo|RESOURCE /photos|routes/web.php->app/Http/Controllers/PhotoController.php".into(),
            format!("RouteRegisteredTo|DELETE /admin/users/{{id}}|routes/web.php->{u}"),
            format!("RouteRegisteredTo|GET /admin/v2/stats|routes/web.php->{u}"),
        ])
    );
    let users = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /users")
        .unwrap();
    assert_eq!(users.to_symbol.as_deref(), Some("UserController.index"));
    assert!(users.evidence.text.starts_with("Route::get('/users'"));
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed path")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn models_commands_and_container_bindings_are_read() {
    let repo = repo(
        &[
            ("composer.json", COMPOSER),
            ("app/Models/User.php", "<?php\nnamespace App\\Models;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass User extends Model {}\n"),
            ("app/Models/Category.php", "<?php\nnamespace App\\Models;\n\nclass Category extends Model {}\n"),
            ("app/Models/Legacy.php", "<?php\nnamespace App\\Models;\n\nclass Legacy extends Model { protected $table = 'old_things'; }\n"),
            ("app/Models/Plain.php", "<?php\nnamespace App\\Models;\n\nclass Plain {}\n"),
            ("app/Console/Commands/SendEmails.php", "<?php\nnamespace App\\Console\\Commands;\n\nclass SendEmails extends Command { protected $signature = 'mail:send {user} {--queue}'; }\n"),
            ("app/Contracts/Mailer.php", "<?php\nnamespace App\\Contracts;\n\ninterface Mailer {}\n"),
            ("app/Services/SmtpMailer.php", "<?php\nnamespace App\\Services;\n\nclass SmtpMailer {}\n"),
            ("app/Providers/AppServiceProvider.php", "<?php\nnamespace App\\Providers;\n\nclass AppServiceProvider {\n    public function register() {\n        $this->app->bind(Mailer::class, SmtpMailer::class);\n    }\n}\n"),
            ("app/Jobs/Notify.php", "<?php\nnamespace App\\Jobs;\n\nclass Notify {\n    public function __construct(private Mailer $mailer, string $name) {}\n}\n"),
            ("app/Http/Controllers/OrderController.php", "<?php\nnamespace App\\Http\\Controllers;\n\nclass OrderController {\n    public function __construct(Plain $plain, SmtpMailer $smtp, Unknown $nothing) {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let mut models: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::ModelMappedToTable)
        .map(|r| (r.label.clone(), r.certainty))
        .collect();
    models.sort();
    assert_eq!(
        models,
        [
            ("categories".to_owned(), Certainty::Convention),
            ("old_things".to_owned(), Certainty::Declared),
            ("users".to_owned(), Certainty::Convention)
        ]
    );
    let command = repo
        .relations()
        .iter()
        .find(|r| r.kind == RelationKind::CommandExposedAs)
        .unwrap();
    assert_eq!(command.label, "mail:send");
    assert_eq!(command.certainty, Certainty::Declared);
    let injections = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("ProviderInjectedInto"))
            .collect(),
    );
    assert_eq!(
        injections,
        sorted(vec![
            "ProviderInjectedInto|SmtpMailer|app/Services/SmtpMailer.php->app/Jobs/Notify.php".into(),
            "ProviderInjectedInto|Plain|app/Models/Plain.php->app/Http/Controllers/OrderController.php".into(),
            "ProviderInjectedInto|SmtpMailer|app/Services/SmtpMailer.php->app/Http/Controllers/OrderController.php".into(),
        ]),
        "a binding injects its concrete class anywhere; other classes are autowired only in controllers"
    );
    let bound = repo
        .relations()
        .iter()
        .find(|r| {
            r.to.as_ref()
                .is_some_and(|t| t.as_path().ends_with("Notify.php"))
        })
        .unwrap();
    assert_eq!(bound.certainty, Certainty::Declared);
    let autowired = repo
        .relations()
        .iter()
        .find(|r| r.label == "Plain")
        .unwrap();
    assert_eq!(autowired.certainty, Certainty::Convention);
}

#[tokio::test]
async fn the_adapter_is_off_without_laravel_and_ignores_files_outside_routes() {
    let off = repo(
        &[
            (
                "composer.json",
                r#"{"name":"x/y","require":{"symfony/framework-bundle":"^7"}}"#,
            ),
            (
                "routes/web.php",
                "<?php\nRoute::get('/x', function () {});\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(off.relations().is_empty());

    let elsewhere = repo(
        &[
            ("composer.json", COMPOSER),
            (
                "app/helpers.php",
                "<?php\nRoute::get('/not-in-routes', function () {});\n",
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
