//! NestJS: controller routes (`@Controller` plus `@Get` and friends) and constructor injection of
//! `@Injectable` providers.
use rusty_repo_model::{
    framework_relations::{
        declares, Certainty, FrameworkAdapter, FrameworkCapability, FrameworkInput,
        FrameworkOutput, FrameworkRelation, RelationDraft, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::{
    decorators::{classes, injections, path_argument, InjectionRules, PathArgument},
    syntax::{evidence, is_script, parse_script},
};

const ID: &str = "nestjs";
const VERSION: u32 = 1;
const ROUTE_RULE: &str = "nestjs-controller-route";
const INJECTION: InjectionRules = InjectionRules {
    framework: ID,
    rule: "nestjs-constructor-injection",
    version: VERSION,
    injectable: "Injectable",
    consumers: &["Controller", "Injectable"],
    inject_function: None,
};
const METHODS: [(&str, &str); 8] = [
    ("Get", "GET"),
    ("Post", "POST"),
    ("Put", "PUT"),
    ("Delete", "DELETE"),
    ("Patch", "PATCH"),
    ("Options", "OPTIONS"),
    ("Head", "HEAD"),
    ("All", "ALL"),
];

pub struct NestjsAdapter;

impl FrameworkAdapter for NestjsAdapter {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: ID.into(),
            name: "NestJS".into(),
            adapter_version: VERSION,
            kinds: vec![RelationKind::RouteRegisteredTo, RelationKind::ProviderInjectedInto],
            handled: vec![
                "@Controller('prefix') / @Controller({ path: 'prefix' }) with @Get, @Post, @Put, @Delete, @Patch, @Options, @Head and @All method decorators and literal paths".into(),
                "constructor parameters typed with an @Injectable class defined in the repository, from @Controller and @Injectable classes".into(),
            ],
            unsupported: vec![
                "global prefixes and versioning set in main.ts".into(),
                "providers and controllers registered in @Module metadata, custom providers (useClass, useFactory, useValue)".into(),
                "@Inject('TOKEN') and forwardRef injection (reported, no relation)".into(),
                "computed controller or method paths (reported, no relation)".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        declares(package, "@nestjs/common").or_else(|| declares(package, "@nestjs/core"))
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        for indexed in input.files_of(package) {
            if cancel.is_cancelled() {
                return Err(DiscoveryError::Cancelled);
            }
            if !is_script(&indexed.path) {
                continue;
            }
            let Some(source) = input.read(&indexed.path) else {
                continue;
            };
            if !source.contains("@Controller") {
                continue;
            }
            let Some(tree) = parse_script(&indexed.path, &source) else {
                continue;
            };
            let file = &indexed.path;
            for class in classes(tree.root_node(), &source) {
                emit_controller_routes(package, file, &source, &class, output);
            }
        }
        injections(package, input, &INJECTION, output);
        Ok(())
    }
}

struct RouteContext<'a, 't> {
    package: &'a Package,
    file: &'a rusty_repo_model::repository_intelligence::RootRelativePath,
    source: &'a str,
    class: &'a crate::decorators::Class<'t>,
    prefix: &'a str,
}

fn emit_controller_routes(
    package: &Package,
    file: &rusty_repo_model::repository_intelligence::RootRelativePath,
    source: &str,
    class: &crate::decorators::Class<'_>,
    output: &mut FrameworkOutput,
) {
    let Some(prefix) = controller_prefix(class, file, source, output) else {
        return;
    };
    let context = RouteContext {
        package,
        file,
        source,
        class,
        prefix: &prefix,
    };
    for method in &class.methods {
        emit_method_routes(&context, method, output);
    }
}

fn controller_prefix(
    class: &crate::decorators::Class<'_>,
    file: &rusty_repo_model::repository_intelligence::RootRelativePath,
    source: &str,
    output: &mut FrameworkOutput,
) -> Option<String> {
    let controller = class.decorator("Controller")?;
    match path_argument(controller, source) {
        PathArgument::None => Some(String::new()),
        PathArgument::Literal(path) => Some(path),
        PathArgument::Computed => {
            output.unresolved(
                file,
                format!(
                    "`{}` has a computed @Controller path; its routes were not read",
                    class.name
                ),
            );
            None
        }
    }
}

fn emit_method_routes(
    context: &RouteContext<'_, '_>,
    method: &crate::decorators::Method<'_>,
    output: &mut FrameworkOutput,
) {
    for decorator in &method.decorators {
        let Some((_, verb)) = METHODS.iter().find(|(name, _)| *name == decorator.name) else {
            continue;
        };
        let Some(subpath) = method_path(context, method, decorator, output) else {
            continue;
        };
        output.relate(FrameworkRelation::new(
            ID,
            Some(&context.package.id),
            RelationDraft {
                kind: RelationKind::RouteRegisteredTo,
                rule: ROUTE_RULE,
                rule_version: VERSION,
                certainty: Certainty::Declared,
                from: context.file.clone(),
                to: Some(context.file.clone()),
                from_symbol: Some(context.class.name.clone()),
                to_symbol: Some(format!("{}.{}", context.class.name, method.name)),
                label: format!("{verb} {}", join_paths(context.prefix, &subpath)),
                evidence: evidence(context.file, decorator.node, context.source),
            },
        ));
    }
}

fn method_path(
    context: &RouteContext<'_, '_>,
    method: &crate::decorators::Method<'_>,
    decorator: &crate::decorators::Decorator<'_>,
    output: &mut FrameworkOutput,
) -> Option<String> {
    match path_argument(decorator, context.source) {
        PathArgument::None => Some(String::new()),
        PathArgument::Literal(path) => Some(path),
        PathArgument::Computed => {
            output.unresolved(
                context.file,
                format!(
                    "`{}.{} ` has a computed @{} path; no relation emitted",
                    context.class.name, method.name, decorator.name
                ),
            );
            None
        }
    }
}

/// `users` and `:id` give `/users/:id`; empty parts are dropped and slashes are not doubled.
fn join_paths(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = [prefix, sub]
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}
