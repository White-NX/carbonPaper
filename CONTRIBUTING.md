# Contribution Guidelines

## Before Start

First, you should clone the source code from the `main` branch. 

When you submit a PR, make sure it goes to the same branch where you originally pulled the code, and clearly explain what logic was added or modified and its effect.

## Start Contribution

To participate this project, you will need these tools:

1. Any kind of IDE.
2. Node.js v22.18.0 or later
3. python v3.12.10 (suggested)

Running this command before developing

```
npm install
```

## Documentation and Comment Language

Use English for new or substantially edited source comments and Rust documentation so
contributors and security reviewers can read the same invariants. User-facing strings
remain localized through the existing i18n system and are not covered by this rule.

When touching a module that contains older Chinese comments, translate nearby comments
that still explain useful intent. Remove comments that merely restate the next line.
Prefer documenting why a constraint exists, which state transition is expected, and what
must remain true across an API or FFI boundary.

Every Tauri command must document its purpose, authentication requirement, parameters,
serialized return shape, and the frontend wrapper or component that calls it. Every new
`unsafe` block or `unsafe impl` must have an adjacent `// SAFETY:` comment explaining the
caller guarantees, pointer/handle validity, ownership, lifetime, and thread-safety
invariants that make the operation sound. CI treats undocumented unsafe blocks as a
Clippy warning; contributors should run `cargo clippy --manifest-path src-tauri/Cargo.toml
--all-targets` before submitting Rust changes.
