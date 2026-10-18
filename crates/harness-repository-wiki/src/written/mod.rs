//! Readable documentation: pages a model writes from the repository's facts and real code, checked
//! so they only name what exists, and marked when the code moves on.
//!
//! The facts layer (`render`) says what is true and where; this layer is what a developer reads.
//! The pages that start a reader off are Overview, Getting started, Key concepts, How it works,
//! Architecture, Active landscape and Glossary (the glossary is mined, never written by a model);
//! then one page per top-level module. Each page gets a fixed shape (`outline`), facts as text,
//! the README's own vocabulary and excerpts of the code it describes (`pages`, `excerpts`,
//! `concepts`, `flow`, `landscape`, `readme`, `scripts`); what comes back is grounded (`ground`).
mod brief;
mod concepts;
mod diagram;
mod evidence;
mod excerpts;
mod flow;
mod freshness;
mod glossary;
mod ground;
mod landscape;
mod model;
mod outline;
mod pages;
mod readme;
mod scripts;

pub use brief::{
    Brief, BriefBuilder, BriefContext, Style, Target, ACTIVE_LANDSCAPE, ARCHITECTURE,
    GETTING_STARTED, GLOSSARY, HOW_IT_WORKS, KEY_CONCEPTS, OVERVIEW,
};
pub use diagram::{dependency_diagram, limit_nodes, Diagram, DiagramNode, MAX_NODES};
pub use freshness::freshness;
pub use ground::{ground, Grounded, Known, DIAGRAM_CLOSE, DIAGRAM_OPEN};
pub use model::{Author, Freshness, InputFile, Source, SourceRef, WrittenKind, WrittenPage};
