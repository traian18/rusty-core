//! NestJS: controller routes (`@Controller` plus `@Get` and friends) and constructor injection of
//! `@Injectable` providers.
use harness_workspace::{
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
                let Some(controller) = class.decorator("Controller") else {
                    continue;
                };
                let prefix = match path_argument(controller, &source) {
                    PathArgument::None => String::new(),
                    PathArgument::Literal(path) => path,
                    PathArgument::Computed => {
                        output.unresolved(
                            file,
                            format!(
                                "`{}` has a computed @Controller path; its routes were not read",
                                class.name
                            ),
                        );
                        continue;
                    }
                };
                for method in &class.methods {
                    for decorator in &method.decorators {
                        let Some((_, verb)) =
                            METHODS.iter().find(|(name, _)| *name == decorator.name)
                        else {
                            continue;
                        };
                        let sub = match path_argument(decorator, &source) {
                            PathArgument::None => String::new(),
                            PathArgument::Literal(path) => path,
                            PathArgument::Computed => {
                                output.unresolved(
                                    file,
                                    format!(
                                        "`{}.{}` has a computed @{} path; no relation emitted",
                                        class.name, method.name, decorator.name
                                    ),
                                );
                                continue;
                            }
                        };
                        output.relate(FrameworkRelation::new(
                            ID,
                            Some(&package.id),
                            RelationDraft {
                                kind: RelationKind::RouteRegisteredTo,
                                rule: ROUTE_RULE,
                                rule_version: VERSION,
                                certainty: Certainty::Declared,
                                from: file.clone(),
                                to: Some(file.clone()),
                                from_symbol: Some(class.name.clone()),
                                to_symbol: Some(format!("{}.{}", class.name, method.name)),
                                label: format!("{verb} {}", join_paths(&prefix, &sub)),
                                evidence: evidence(file, decorator.node, &source),
                            },
                        ));
                    }
                }
            }
        }
        injections(package, input, &INJECTION, output);
        Ok(())
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
