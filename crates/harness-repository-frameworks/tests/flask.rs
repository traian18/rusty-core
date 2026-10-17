mod support;

use std::sync::Arc;

use harness_repository_frameworks::FlaskAdapter;
use harness_workspace::framework_relations::FrameworkAdapter;
use support::*;

const PYPROJECT: &str = "[project]\nname = \"web\"\ndependencies = [\"Flask>=2\"]\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(FlaskAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn routes_blueprints_shortcuts_and_add_url_rule_are_read() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("web/__init__.py", ""),
            ("web/app.py", "from flask import Flask\nfrom web.views import index\napp = Flask(__name__)\n\n@app.route(\"/\")\ndef home():\n    pass\n\n@app.route(\"/submit\", methods=[\"POST\", \"put\"])\ndef submit():\n    pass\n\n@app.get(\"/ping\")\ndef ping():\n    pass\n\napp.add_url_rule(\"/index\", view_func=index, methods=[\"GET\"])\n"),
            ("web/views.py", "def index():\n    pass\n"),
            ("web/admin.py", "from flask import Blueprint\nbp = Blueprint('admin', __name__, url_prefix='/admin')\n\n@bp.route('/users')\ndef users():\n    pass\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /|web/app.py->web/app.py".into(),
            "RouteRegisteredTo|POST /submit|web/app.py->web/app.py".into(),
            "RouteRegisteredTo|PUT /submit|web/app.py->web/app.py".into(),
            "RouteRegisteredTo|GET /ping|web/app.py->web/app.py".into(),
            "RouteRegisteredTo|GET /index|web/app.py->web/views.py".into(),
            "RouteRegisteredTo|GET /admin/users|web/admin.py->web/admin.py".into(),
        ])
    );
    let index = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /index")
        .unwrap();
    assert_eq!(index.to_symbol.as_deref(), Some("index"));
    assert!(index.rule.id == "flask-add-url-rule");
}

#[tokio::test]
async fn look_alikes_and_unprovable_receivers_emit_nothing() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("web/other.py", "from flask import request\nfrom elsewhere import bp\n\n@bp.route('/imported')\ndef a():\n    pass\n\n@cache.get('nothing')\ndef b():\n    pass\n\ndef c():\n    return request.args.get('x')\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert_eq!(repo.diagnostics().len(), 1, "{:?}", repo.diagnostics());

    let off = self::off().await;
    assert!(off.relations().is_empty());
}

async fn off() -> Repo {
    repo(
        &[
            ("pyproject.toml", "[project]\nname = \"x\"\ndependencies = [\"django\"]\n"),
            ("a.py", "from flask import Flask\napp = Flask(__name__)\n@app.route('/x')\ndef x():\n    pass\n"),
        ],
        adapters(),
    )
    .await
}
