//! Per-language syntax rules and resolution schemes.

use std::sync::Arc;

use super::collector::Syntax;
use super::resolve::{LanguageResolver, SchemeResolver};
use crate::model::LanguageId;

mod cpp;
mod csharp;
mod go;
mod java;
mod javascript;
mod kotlin;
mod php;
mod python;
mod ruby;
mod rust;
mod util;

pub(crate) fn syntax_for(language: &LanguageId) -> Box<dyn Syntax> {
    match language {
        LanguageId::JavaScript => Box::new(javascript::Js { typescript: false }),
        LanguageId::TypeScript => Box::new(javascript::Js { typescript: true }),
        LanguageId::Python => Box::new(python::Python),
        LanguageId::Java => Box::new(java::Java),
        LanguageId::Kotlin => Box::new(kotlin::Kotlin),
        LanguageId::Go => Box::new(go::Go),
        LanguageId::Rust => Box::new(rust::Rust),
        LanguageId::Php => Box::new(php::Php),
        LanguageId::Cpp => Box::new(cpp::Cpp),
        LanguageId::Ruby => Box::new(ruby::Ruby),
        LanguageId::CSharp => Box::new(csharp::CSharp),
    }
}

pub(crate) fn resolver_for(language: &LanguageId) -> Arc<dyn LanguageResolver> {
    match language {
        LanguageId::JavaScript => {
            Arc::new(SchemeResolver(javascript::JsScheme { typescript: false }))
        }
        LanguageId::TypeScript => {
            Arc::new(SchemeResolver(javascript::JsScheme { typescript: true }))
        }
        LanguageId::Python => Arc::new(SchemeResolver(python::PythonScheme)),
        LanguageId::Java => Arc::new(SchemeResolver(java::JavaScheme)),
        LanguageId::Kotlin => Arc::new(SchemeResolver(kotlin::KotlinScheme)),
        LanguageId::Go => Arc::new(SchemeResolver(go::GoScheme)),
        LanguageId::Rust => Arc::new(SchemeResolver(rust::RustScheme)),
        LanguageId::Php => Arc::new(SchemeResolver(php::PhpScheme)),
        LanguageId::Cpp => Arc::new(SchemeResolver(cpp::CppScheme)),
        LanguageId::Ruby => Arc::new(SchemeResolver(ruby::RubyScheme)),
        LanguageId::CSharp => Arc::new(SchemeResolver(csharp::CSharpScheme)),
    }
}
