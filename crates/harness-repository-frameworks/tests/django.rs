mod support;

use std::sync::Arc;

use harness_repository_frameworks::DjangoAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const PYPROJECT: &str = "[project]\nname = \"site\"\ndependencies = [\"Django>=4\"]\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(DjangoAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn url_patterns_name_their_views() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("blog/__init__.py", ""),
            ("blog/urls.py", "from django.urls import path, re_path, include\nfrom . import views\nfrom .views import ArticleView, detail\n\nbase = 'dyn/'\nurlpatterns = [\n    path('', views.index, name='index'),\n    path('articles/<int:pk>/', detail),\n    path('classes/', ArticleView.as_view()),\n    re_path(r'^legacy/$', views.legacy),\n    path('api/', include('blog.api.urls')),\n    path(base, views.index),\n]\n"),
            ("blog/views.py", "def index(request):\n    pass\n\ndef detail(request, pk):\n    pass\n\ndef legacy(request):\n    pass\n\nclass ArticleView:\n    @classmethod\n    def as_view(cls):\n        pass\n"),
            ("blog/helpers.py", "def path(x):\n    return x\n\ndata = path('not-a-route')\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|ANY /|blog/urls.py->blog/views.py".into(),
            "RouteRegisteredTo|ANY /articles/<int:pk>/|blog/urls.py->blog/views.py".into(),
            "RouteRegisteredTo|ANY /classes/|blog/urls.py->blog/views.py".into(),
            "RouteRegisteredTo|ANY /legacy/|blog/urls.py->blog/views.py".into(),
        ])
    );
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed pattern")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn models_map_to_tables_and_commands_to_their_names() {
    let repo = repo(
        &[
            ("pyproject.toml", PYPROJECT),
            ("blog/__init__.py", ""),
            ("blog/models.py", "from django.db import models\n\nclass Article(models.Model):\n    title = models.CharField(max_length=10)\n\nclass Author(models.Model):\n    class Meta:\n        db_table = 'writers'\n\nclass Base(models.Model):\n    class Meta:\n        abstract = True\n\nclass Plain:\n    pass\n"),
            ("blog/management/__init__.py", ""),
            ("blog/management/commands/__init__.py", ""),
            ("blog/management/commands/import_articles.py", "from django.core.management.base import BaseCommand\n\nclass Command(BaseCommand):\n    def handle(self, *args, **options):\n        pass\n"),
        ],
        adapters(),
    )
    .await;
    let models: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::ModelMappedToTable)
        .map(|r| (r.label.clone(), r.from_symbol.clone().unwrap(), r.certainty))
        .collect();
    assert_eq!(
        models,
        [
            (
                "blog_article".to_owned(),
                "Article".to_owned(),
                Certainty::Convention
            ),
            (
                "writers".to_owned(),
                "Author".to_owned(),
                Certainty::Declared
            )
        ]
    );
    let command = repo
        .relations()
        .iter()
        .find(|r| r.kind == RelationKind::CommandExposedAs)
        .unwrap();
    assert_eq!(command.label, "import_articles");
    assert_eq!(command.certainty, Certainty::Convention);
}

#[tokio::test]
async fn the_adapter_is_off_without_django() {
    let repo = repo(
        &[
            (
                "pyproject.toml",
                "[project]\nname = \"x\"\ndependencies = [\"flask\"]\n",
            ),
            ("app/urls.py", "urlpatterns = []\n"),
            ("app/models.py", "class A(models.Model):\n    pass\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
}
