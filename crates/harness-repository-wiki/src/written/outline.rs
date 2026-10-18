//! What each page must say and how. Pages are written for a developer who has never seen the
//! repository: concrete, explanatory prose built from the code they will read, not a list of
//! references. These texts are this project's own.
use super::model::WrittenKind;

/// The rules every page follows.
pub fn system(style: &str, diagrams: &[String]) -> String {
    format!(
        "You write the documentation a developer reads to understand a codebase they have never seen. You are given facts computed from the repository index and excerpts of its real code.\n\n\
How to write:\n\
- Explain. Say what each part does, why it exists, and how the parts work together, in plain sentences a newcomer can follow. A reader should finish the page able to explain the system to someone else.\n\
- Be concrete. Name the real types, functions and files the excerpts show, and say what they do. Never describe a part only by where it is (\"files in src/x\"); describe what it does.\n\
- Use only what the facts and excerpts establish. Never invent a file, symbol, command, version, number or reason. If something is not shown, leave it out rather than guess.\n\
- The README digest is the project's own vocabulary: use its names for things. Take paths, counts and structure only from the facts, which describe the code as it is today.\n\
- Name a file or folder only as a backticked repository-relative path (`src/engine/run.rs`) and a code symbol only in backticks (`SessionEngine`). Never write Markdown links. Backticked names the evidence does not contain are removed from the page.\n\
- No footnote numbers, no bibliography, no closing summary, no preamble. Output only the page, in Markdown.\n\
- Headings name what the section explains (\"From a request to a stored result\"), never generic labels such as Overview, Details, Summary or Files.\n\
- Short paragraphs, active voice, third person. At most one table unless the outline asks for one.\n\
- Diagram markers (`@@diagram:KEY@@`) go on a line of their own exactly where the outline puts them; the diagram is drawn for you, so describe what it shows in the prose next to it. Allowed: {}.\n\
- Style: {style}.",
        if diagrams.is_empty() { "none".to_owned() } else { diagrams.join(", ") }
    )
}

pub fn style_guidance(name: &str) -> &'static str {
    match name {
        "reference" => "terse and complete: short sentences, every fact a developer needs, no background",
        "tutorial" => "ordered by what a newcomer needs first: start from the big picture and lead the reader step by step",
        _ => "clear and explanatory: say what each part is for and why it is shaped the way the code shows",
    }
}

/// The page's shape: sections, their order, and how long it should be.
pub fn instructions(kind: WrittenKind, name: &str, has_diagram: bool) -> String {
    let diagram = |under: &str| {
        if has_diagram {
            format!("\nPut `@@diagram:{under}@@` on its own line right after this section's first paragraph.")
        } else {
            String::new()
        }
    };
    match kind {
        WrittenKind::Overview => format!(
            "Write the front page of the documentation for {name}: the first page anyone opens. It answers \"what is this and how is it put together\" before anything else.\n\n\
## What {name} is\nTwo short paragraphs. The first sentence says what the project does end to end: what it takes in, what it does with it, and what it produces. Then who uses it and for what. Do not open with numbers or a list of folders.\n\n\
## How it is put together\nThe parts and what each one is for, one or two sentences each, in the project's own vocabulary (\"the engine that runs agent sessions\", not \"holds Rust files\"). Then how work actually moves through them. Distinguish the path that builds or updates what the system stores (indexing, compiling, syncing) from the path that uses it (serving, querying, showing it to a user), when the facts support both.{}\n\n\
## Key concepts\nFive to seven named ideas a reader must learn to read this code, as bullets: **Name** followed by one or two sentences on what it is and where it lives. A concept is a word the project has for something (a layer, a store, a kind of record, a protocol), not a folder.\n\n\
## Where to go next\nThree or four bullets pointing the reader to the right next page or file for common goals (run it, change a feature, understand a flow).\n\n\
Length: 650 to 850 words.",
            diagram("components")
        ),
        WrittenKind::GettingStarted => format!(
            "Write the Getting started page for {name}: what a developer does to get it running on their machine.\n\n\
## Prerequisites\nBullets of the tools and runtimes the setup sections or manifests establish. Say when a version is not documented; never guess one.\n\n\
## Install\nNumbered steps, each with the folder to run it in and a fenced shell block, only with commands that appear word for word in the setup sections, or `npm run <key>` / `make <target>` for a key listed under declared scripts.\n\n\
## Run\nHow to start it, by the same rule. Name the entry point files where it starts.\n\n\
## Test\nHow to run the tests, by the same rule.\n\n\
## Check that it works\nWhat a developer should see when it works, only as far as the setup text says.\n\n\
## Troubleshooting\nAt most two bullets, only if the setup text mentions a common problem; otherwise leave this section out.\n\n\
Never invent or tidy up a command; a command not in the evidence must not appear. If the repository documents no install step, say so and name the file a developer should read. Length: 400 to 700 words, shorter when the repository documents less."
        ),
        WrittenKind::KeyConcepts => format!(
            "Write the Key concepts page for {name}: the mental model a developer needs before reading the code.\n\n\
## What you are about to learn\nTwo or three sentences: how many concepts matter and what kind of system they make up.\n\n\
## Core concepts\nOne `### `Name`` subsection per concept from the candidate list, four to seven of them, chosen so they cover the system rather than one corner of it. For each: a paragraph of two to four sentences on what it is and the role it plays, based on its excerpt, then a line `**Where it lives:** `path``.\n\n\
## How they connect\nOne paragraph tracing how the concepts work together. Claim that one uses another only where the listed links say so; otherwise describe how they relate by layer or folder.\n\n\
Use only concepts from the candidate list. Length: 500 to 800 words."
        ),
        WrittenKind::HowItWorks => format!(
            "Write the How it works page for {name}: one concrete walk through the system, step by step, like following a request through it.\n\n\
## In one sentence\nWhat the system does, in a single concrete sentence.\n\n\
## A walk through one example\nThe heart of the page. Start where the traced path starts (the entry point) and follow it file by file, saying at each step what that code does with what it was handed, using the excerpts. Say \"when you run…\" for a tool, \"when a request reaches…\" for a service, \"when a caller uses…\" for a library. Only say that one part calls another where an excerpt shows the call; otherwise describe each step on its own.\n\n\
## Why it is shaped this way\nTwo or three sentences on why this structure fits what the system is, from what the path shows. No invented history.\n\n\
## Where to read next\nTwo to four bullets: a backticked file from the path and half a sentence on why to open it.\n\n\
Length: 500 to 800 words."
        ),
        WrittenKind::Architecture => format!(
            "Write the Architecture page for {name}: how the system is divided and how the divisions work together.\n\n\
## The parts and what they own\nEach component and what it is responsible for, one short paragraph each, from its facts and excerpts, in the project's own vocabulary.{}\n\n\
## How work moves through the system\nThe main paths through the parts, in order. Keep the path that builds or maintains stored state (indexing, syncing, migrations, background jobs) apart from the path that serves or uses it (requests, commands, the user interface), and say where they meet.\n\n\
## Boundaries and rules\nWhich parts may depend on which, the direction of the dependencies, cycles or boundary problems the facts list, and what they mean for someone changing the code.\n\n\
## Where the weight is\nThe files and parts most of the system depends on, and why changes there reach far.\n\n\
Length: 600 to 900 words.",
            diagram("components")
        ),
        WrittenKind::ActiveLandscape => format!(
            "Write the Active landscape page for {name}: where work is happening now, like a hand-over to someone joining the team this week.\n\n\
## Where work is concentrated\nTwo or three sentences naming the busiest folders and what kind of work the facts suggest. Do not invent features or commit messages.\n\n\
## Files in motion\nBullets, one per busy file: who changes it, and one plain observation about why it is busy (a central file most features pass through, a large file under refactoring, a file that keeps getting fixed), only as far as the facts show.\n\n\
## Steady ground\nOne or two sentences on how much of the code is stable and therefore safe to learn first.\n\n\
## Watch out for\nBullets from the code-health findings on the busy files. If there are none, say so in one line.\n\n\
Matter of fact, no marketing. Length: 300 to 550 words."
        ),
        WrittenKind::Module => format!(
            "Write the page for the module {name}: a newcomer should understand how it works in five minutes.\n\n\
Start with two or three plain sentences, without a heading: what this part of the system does, what it takes in and hands on, and why the rest of the system needs it. No paths, numbers or identifiers in this opening, and do not open with \"This module…\" or \"X is the Y of Z\".{}\n\n\
Then three to five `##` sections, each named after a step or an idea in how the module works (\"From a file to parsed symbols\"), never a generic name. Each section is one to three short paragraphs explaining how and why, naming the types and functions the excerpts show. End each section with a line `Sources: ` and one to three backticked file paths that section talks about.\n\n\
## Where to start reading\nExactly three bullets: a backticked path and one sentence on why to read it first.\n\n\
## How it connects\nOne short paragraph: what depends on this module and what it depends on.\n\n\
No lists of symbols, no counts of files or commits. Length: 500 to 800 words.",
            if has_diagram { "\n\nThen `@@diagram:structure@@` on its own line." } else { "" }
        ),
        WrittenKind::Glossary | WrittenKind::File => String::new(),
    }
}
