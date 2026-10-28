# A practical guide to good Rust

There is no single architecture or style that every Rust project should follow. But there is a strong mainstream consensus: **make ownership, failure, and invariants clear; use the type system to prevent invalid states; follow conventional formatting and naming; and prefer simple, predictable APIs over clever abstractions.**

The guidance below draws primarily on *The Rust Programming Language* (“the Rust Book”), the Rust API Guidelines, and Clippy. The API Guidelines are recommendations—not rules—and some apply mainly to libraries rather than applications.

## 1. Make ownership and borrowing intentional

Rust code is easier to understand when a function’s signature tells the reader what happens to its inputs.

- Use `&T` when a function needs to read a value without keeping it.
- Use `&mut T` when it needs to modify a value owned by the caller.
- Take `T` by value when the function needs to consume it, transform it, or keep ownership.
- Return owned data when callers need to retain the result independently.
- Reach for `Rc<T>` or `Arc<T>` when shared ownership is part of the design—not just to avoid thinking about ownership.

For example:

```rust
fn display_name(name: &str) {
    println!("{name}");
}

fn normalize_name(name: &mut String) {
    *name = name.trim().to_lowercase();
}

fn save_name(name: String) {
    // This function takes ownership of the name.
}
```

Borrowing a `String` as `&str` is often useful when the function only needs text. It lets callers pass either a borrowed string or a string slice without requiring the function to take ownership.

Do not treat cloning as inherently wrong. A clone can make ownership simpler and the code easier to maintain. But if a clone is frequent or on a hot path, understand what is being copied and whether borrowing or moving the value would be clearer or cheaper.

**Rule of thumb:** a reader should be able to look at a function signature and tell whether the function reads, modifies, consumes, or returns its data.

## 2. Represent absence and failure in the type

Rust makes two common possibilities explicit:

- `Option<T>` means a value may be present or absent.
- `Result<T, E>` means an operation may succeed or fail.

```rust
fn find_user(id: UserId) -> Option<User> {
    // `None` means there is no matching user.
    todo!()
}

fn load_config(path: &Path) -> Result<Config, ConfigError> {
    // `Err` describes why loading failed.
    todo!()
}
```

This is generally clearer than sentinel values such as an empty string, `-1`, or a special “not found” object. It also helps callers handle the cases deliberately:

```rust
let user = match find_user(id) {
    Some(user) => user,
    None => return Err(AppError::UserNotFound(id)),
};
```

Use `?` when an error should propagate to the caller:

```rust
fn read_config(path: &Path) -> Result<Config, ConfigError> {
    let contents = std::fs::read_to_string(path)?;
    parse_config(&contents)
}
```

At application boundaries, errors often need to be reported to a person, logged, or translated into an exit status. That is a good place to decide how to present or handle them. Lower-level functions can usually return errors and let their callers choose what to do.

### When is `panic!` appropriate?

A useful default is:

- Return `Result` for failures that are expected or that a caller could reasonably handle.
- Use `panic!` when a program has reached a broken invariant or an unrecoverable state.
- Use `expect` when you can justify why a failure is impossible in that specific context.
- `unwrap` is often reasonable in tests and short examples; it is usually a poor way to handle routine failures in production code.

For example, malformed user input should normally produce an error, not crash the program. A failure to parse a hard-coded string that the code itself guarantees is valid may justify an `expect` with a specific explanation.

**Rule of thumb:** do not panic merely because handling an error is inconvenient. But do not pretend the program can safely continue after an invariant has been violated.

## 3. Use types to express important rules

A type can make a requirement visible in a function signature instead of relying on comments or repeated checks.

Suppose a function requires a non-empty name. Passing a plain `String` does not express that requirement. A validated newtype can:

```rust
pub struct NonEmptyName(String);

impl NonEmptyName {
    pub fn parse(value: String) -> Result<Self, NameError> {
        if value.trim().is_empty() {
            return Err(NameError::Empty);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn create_account(name: NonEmptyName) {
    // The name has already passed validation.
}
```

Newtypes are useful when two values have the same underlying representation but different meanings—for example, `UserId` and `OrderId`, both backed by integers. They are not automatically worthwhile for every primitive value; use them when the distinction prevents real mistakes or clarifies the domain.

Enums are similarly useful for representing alternatives:

```rust
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected { address: String },
}
```

This is often safer than several booleans such as `is_connected`, `is_connecting`, and `has_address`, because the enum does not allow contradictory combinations.

### Do not overuse typestate

Typestate can encode operation sequences in types—for example, distinguishing an unconfigured connection from a ready one. It can be powerful when incorrect sequencing would be dangerous or confusing. But it can also make a small API harder to understand. Prefer the simpler design unless the type-level constraint meaningfully improves safety or usability.

## 4. Design APIs to be predictable

Rust APIs are easier to use when names and behavior fit standard expectations.

- Use `snake_case` for functions, methods, modules, and variables.
- Use `UpperCamelCase` for types, traits, and enum variants.
- Use `SCREAMING_SNAKE_CASE` for constants and statics.
- Follow familiar conversion naming: `as_...` for cheap borrowed conversions, `to_...` for conversions that may allocate, and `into_...` for conversions that take ownership.
- Use `iter`, `iter_mut`, and `into_iter` for the corresponding collection iteration styles.
- Prefer standard traits such as `From`, `AsRef`, `Debug`, `Clone`, `PartialEq`, and `Default` when their semantics fit.

For example, implement `From<RawId> for UserId` when every `RawId` is a valid `UserId`. If conversion can fail, use `TryFrom` or a clearly named parsing or validation function instead.

A public method should behave the way its name suggests. Avoid surprising side effects, hidden I/O, unexpected blocking, or silently ignored failures. If a call may panic, block, allocate heavily, or perform an external operation, make that expectation clear in its documentation and API shape.

Avoid overly generic functions when they make the API harder to use. Generics are helpful when callers benefit from flexibility; they are not a goal in themselves.

### Public APIs deserve extra care

Public types and functions are commitments to users. Before exposing something, consider whether callers actually need it. Private implementation details are easier to change later.

For a library, document the behavior users rely on: what the function does, what errors it returns, whether it may panic, and any important constraints. Include examples when they make correct use easier to understand. The Rust API Guidelines recommend that examples use `?` rather than presenting `unwrap` as the normal way to handle failure.

## 5. Format conventionally; let tools do the mechanical work

Rust developers generally expect code to be formatted with `rustfmt`. Formatting by hand is a poor use of review time: consistent layout makes substantive changes easier to see.

Idiomatic naming and formatting are predictable enough that they should rarely need discussion in a code review. Use descriptive names, avoid unexplained abbreviations, and keep functions focused on one coherent task. Split a function when doing so clarifies the steps or creates a useful unit of reuse—not just to meet an arbitrary line count.

Clippy catches common mistakes and suggests more idiomatic or efficient alternatives. Treat its output as useful evidence, not absolute authority: a lint suggestion may not fit a particular API or project constraint. Clippy’s restrictive lint group is explicitly not intended to be enabled wholesale; individual lints can be useful when a project has a concrete reason for them.

## 6. Organize modules around responsibility

Rust’s module system provides both organization and privacy. Modules should help a reader find related behavior and understand which implementation details are public.

For a small project, one or a few modules may be enough. As it grows, group related types and functions by coherent responsibility. Prefer clear boundaries such as `config`, `parser`, or `storage` over either a single enormous module or a large collection of tiny files whose boundaries add no value.

A common application shape is:

- **Core or domain logic** for the rules and decisions central to the application.
- **Adapters** for external systems such as files, databases, networks, or user interfaces.
- **A thin entry point** that parses process-level inputs, wires components together, and reports the final result.

This is a useful starting point, not a required pattern. Small applications may not benefit from a formal “clean architecture,” and libraries have different needs from executables.

### When should a project use multiple crates?

A Cargo package can contain a library crate and binary crate. A binary can keep its process-level startup in `main` and put reusable or testable application logic in the library. Larger related packages can live in a workspace.

Create a crate boundary when it provides a meaningful benefit—for example, independent reuse, a clear dependency boundary, a distinct build target, or a useful separation between components. Do not split a project into many crates merely to make the directory tree look more architectural.

The Rust Book’s general guidance is to split growing programs into modules and files to clarify where functionality lives; workspaces are useful when multiple related packages evolve together.

## 7. Keep errors useful and layered

A good error tells the right audience what went wrong and preserves enough information to act on it.

At a lower level, an error might describe a failed parse or file operation. At a higher level, the application may add context such as which configuration file was being loaded. Avoid replacing every failure with a vague message like “something went wrong,” and avoid exposing internal implementation details that callers cannot use.

Different projects choose different error-type designs. A library may want specific error types so callers can match on cases. An application may favor a simpler error-reporting strategy at its outer boundary. The important point is that the choice should fit who needs to handle the errors.

## 8. Test behavior at the right boundaries

Rust’s usual testing vocabulary distinguishes:

- **Unit tests**, which are small and focused and can test private implementation details.
- **Integration tests**, which use the library as an external caller would.

Use unit tests for logic that is easiest to test near its implementation. Use integration tests to check public behavior and important interactions between components. Tests should make failures understandable: a useful assertion message and a small focused case are often more helpful than a large test covering many unrelated behaviors.

Test meaningful edge cases, especially around parsing, boundaries, missing data, and failure paths. But do not write tests that simply duplicate the implementation line by line; tests should establish behavior callers rely on.

For libraries, documentation examples can also serve as compiled examples, making them valuable both to users and maintainers.

## 9. Be cautious with shared state and concurrency

Rust’s ownership rules help enforce safe access to data, but they do not automatically make an architecture understandable.

- Use `&mut T` when a single owner needs mutable access.
- Use `Mutex<T>` or `RwLock<T>` when synchronized shared mutation is genuinely needed.
- Use `Arc<T>` when shared ownership across threads is needed.
- Prefer message passing when it makes ownership and work coordination clearer.

A program that compiles safely can still have hard-to-understand lock ordering, excessive contention, or unclear ownership responsibilities. Choose the least complicated synchronization design that meets the requirement.

## 10. Treat unsafe code as a contract

`unsafe` is not inherently bad, but it shifts more responsibility to the programmer. Use it only where safe Rust cannot express what is needed, and keep the unsafe block small and close to the operation requiring it.

Document the safety assumptions so future maintainers can verify them. Prefer a small, well-reviewed unsafe boundary over spreading unsafe operations throughout ordinary application logic. A safe public API should uphold its guarantees for callers who use it correctly.

## 11. Common traps to avoid

- **Cloning by default:** it can obscure ownership or add cost. Clone when it is the clearest appropriate choice, not automatically.
- **Panicking for ordinary failure:** user input, missing files, and network errors are normally recoverable possibilities.
- **Overusing `Arc<Mutex<_>>`:** shared mutable state can be a sign that ownership or task boundaries need clarification.
- **Making everything public:** broad public APIs are harder to evolve.
- **Abstracting too early:** a trait or generic framework should solve a real use case, not just make code look reusable.
- **Creating too many crates or layers:** structure should make changes easier, not add ceremony.
- **Following every lint blindly:** lints are advice that needs context, especially stricter lint groups.
- **Treating one architecture as universal:** a CLI, embedded program, library, and server have different constraints.

## A compact review checklist

When reviewing Rust code, ask:

1. **Ownership:** Is it clear who owns each value and why a borrow or move is used?
2. **Failure:** Are expected failures represented with `Result` or `Option` and handled at an appropriate boundary?
3. **Invariants:** Could a useful type prevent invalid values or contradictory states?
4. **Clarity:** Are names, signatures, and control flow conventional and unsurprising?
5. **API:** Is the public surface no larger than necessary, and is its behavior documented?
6. **Structure:** Do modules and crates correspond to useful responsibilities?
7. **Tests:** Are important public behaviors and failure cases covered?
8. **Tools:** Is the code formatted, and have relevant Clippy lints been considered?
9. **Complexity:** Is each abstraction earning its place?
10. **Safety:** If unsafe code exists, are its assumptions explicit and tightly contained?

A strong practical summary is: **make the important rules visible in types and signatures, return errors when callers may recover, keep public APIs predictable, and use the simplest structure that keeps responsibilities clear.**

### Sources

- [The Rust Programming Language: Packages, Crates, and Modules](https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-crates-and-modules.html)
- [The Rust Programming Language: Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [The Rust Programming Language: To `panic!` or Not to `panic!`](https://doc.rust-lang.org/book/ch09-03-to-panic-or-not-to-panic.html)
- [The Rust Programming Language: Improving Error Handling and Modularity](https://doc.rust-lang.org/book/ch12-03-improving-error-handling-and-modularity.html)
- [The Rust Programming Language: Test Organization](https://doc.rust-lang.org/book/ch11-03-test-organization.html)
- [Rust API Guidelines Checklist](https://rust-lang.github.io/api-guidelines/checklist.html)
- [Clippy Documentation](https://doc.rust-lang.org/clippy/)