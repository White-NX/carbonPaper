# Contribution Guidelines

## Before Start

First, you should clone the source code from the `main` branch. 

When you submit a PR, make sure it goes to the same branch where you originally pulled the code, and clearly explain what logic was added or modified and its effect.

## Start Contribution

To participate this project, you will need these tools:

1. Any kind of IDE.
2. Node.js v22.18.0 or later
3. Rust stable toolchain

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

## Database Journal Mode

The main screenshot database defaults to WAL in both development and release builds.
Existing DELETE databases are converted at startup before application connections become
available. WAL lets independent readers coexist with capture commits; operations sharing
the primary connection still serialize through its mutex. Both modes retain
`synchronous=FULL` for commit durability.

To request DELETE for compatibility or troubleshooting, set the process environment
variable before starting the app (the legacy variable name is retained for compatibility):

```powershell
$env:CARBONPAPER_WAL_EXPERIMENT = "0"
npm run debug
```

The value `1` explicitly selects WAL. An unset, empty, or invalid value uses the default
WAL policy; invalid values also produce a warning. To restore the default in PowerShell,
run `Remove-Item Env:CARBONPAPER_WAL_EXPERIMENT -ErrorAction SilentlyContinue` and restart.
The policy is resolved once per process and also applies after backup restore or a data
directory change. A maintenance command that switches to DELETE changes the current
database mode; keep the `0` override set if subsequent opens should continue using DELETE.

Conversions require a local, writable database directory and free space of at least twice
the database file group's size plus 64 MiB. If a supported conversion cannot complete,
startup retains the effective journal mode, records the failure, and retries on a later
initialization. Already-WAL startup does not require this conversion space.

Backups and directory migrations drain database activity and preserve `screenshots.db`,
`screenshots.db-wal`, and `screenshots.db-shm` together. Use the app's backup flow for a
running database. Automatic checkpoints run at 1,000 pages, and the retained journal size
limit is 64 MiB after checkpoints; active readers or large transactions can temporarily
grow the WAL beyond that limit. Startup and full VACUUM maintenance also reclaim WAL space.
The processing staging database and app-bound service ledger keep their own DELETE policy.

Run `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-wal-foundation.ps1`
for the WAL regression checks, or add `-AllRust` for the complete Rust library suite.

Every Tauri command must document its purpose, authentication requirement, parameters,
serialized return shape, and the frontend wrapper or component that calls it. Every new
`unsafe` block or `unsafe impl` must have an adjacent `// SAFETY:` comment explaining the
caller guarantees, pointer/handle validity, ownership, lifetime, and thread-safety
invariants that make the operation sound. CI treats undocumented unsafe blocks as a
Clippy warning; contributors should run `cargo clippy --manifest-path src-tauri/Cargo.toml
--all-targets` before submitting Rust changes.
