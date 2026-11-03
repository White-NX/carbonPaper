# Storage encryption domains

CarbonPaper retains the public-derived SQLCipher bootstrap key so capture can
write during a locked cold start. That outer key is an opening mechanism, not a
confidentiality boundary. Sensitive derived content now has independent inner
encryption, and the saved public key must match the existing Windows CNG key
before it can wrap new content keys.

## Protected content and key authority

| Domain | Content | Persistence |
| --- | --- | --- |
| `metadata` | Smart Cluster anchor/display text; titles, summaries, OCR summaries, key points, evidence and summary source hashes; classification anchors including learned vectors; process labels | AES-256-GCM envelopes inside SQLCipher |
| `vectors` | Derived embeddings, Smart Cluster anchor vectors and frozen ANN construction inputs | AES-256-GCM envelopes inside SQLCipher |
| `ann` | Flat vector records, serialized HNSW graphs and graph checkpoints | Authenticated encrypted sidecars |

Screenshot images, OCR text and other screenshot payloads retain their existing
public-key-wrapped row encryption. Both capture writers store opaque process
tokens for grouping; the separately encrypted catalog supplies labels after
authorization. Filters and deletion resolve the same process across key epochs.

`credential_public_key.bin` is compared byte for byte with the public blob
exported through an **existing-only, silent** CNG open. A cached authority still
checks subsequent file reads, catching replacement during an unlocked session.
Only first-use setup can create a missing CNG key. Existing datasets with a
missing public/master file or mismatched public material fail initialization;
setup does not replace their credentials. The master file is prepared before a
new database is created. It is wrapped using validated exported public material,
so that path cannot create a CNG key or open a private-key authentication prompt.
Initial public/master-file publication is synced and refuses to replace an
existing file; a losing initializer does not cache its unpublished master key.

Recovery must restore the original matching public/master material and dataset
from a trusted source. A missing private CNG key requires its own Windows key
recovery; changing the public file cannot recover existing ciphertext. Credential
validation and first-use creation remain separate operations.

Implementation: [credential authority](../src-tauri/src/credential_manager.rs),
[protected storage](../src-tauri/src/storage/protected.rs), and
[envelopes](../src-tauri/app-bound/src/payload.rs).

## Write provenance and unattended processing

Each process creates three fresh random write keys and wraps them to the
validated CNG public key. Ordinary future writes never choose a key from the
caller-writable persisted envelope registry. This matters because anybody with
the public key can wrap a key they already know.

Historical keys are read through the retained authenticated CNG handle or an
explicit grant from the verified app-bound service. The service binds each grant
to the owner, registered runtime, task dataset, stable content dataset, public
fingerprint, purpose, key ID and wrapped-key digest. It stores keys under the
existing client/SYSTEM DPAPI protection and pins the public fingerprint and
content identity for that dataset.

Runtime-generated grants have `write_origin=true`. Registering an envelope
unwrapped during an authenticated historical read creates a read grant and
cannot promote it to a writer. Resuming an earlier ANN build requires a service
grant confirming original write provenance. Without it, construction restarts
with a fresh key; an injected, public-wrapped known key cannot become the next
index writer.

Disablement or dataset retirement removes service key blobs but retains the
original binding, key digest and provenance. A public writer cannot revive that
grant. An authenticated read can restore exactly the original key, preserving
its original provenance. Historical domain grants last until explicit
revocation; the pending-input retention window does not expire history keys.
Services lacking the new storage-key capability cannot authorize these reads.

Two dataset identities serve different lifetimes:

- `app_bound_dataset_id` is the resettable service/task scope.
- `protected_content_dataset_id` is the stable inner-encryption/AAD scope.

A background-dataset reset creates new write keys and service bindings while
preserving the second identity, so encrypted history remains readable. These
grants expose derived domains to the approved processing runtime; they do not
give it the archive master key or CNG private key.

Implementation: [service grants](../src-tauri/app-bound/src/ledger/storage_keys.rs),
[staging transport](../src-tauri/src/processing_stage.rs), and
[dataset reset](../src-tauri/src/storage/processing_stage.rs).

## Vector performance and cache lifecycle

The `CPDVEC05` flat file keeps ciphertext in its mmap. Each vector record is
decrypted and authenticated only when read. The HNSW graph is decrypted once
into anonymous owned memory before USearch opens its view, avoiding a plaintext
temporary graph file. Both the header and vector records authenticate the
snapshot; record AAD also binds ordinal and subject identity. Worker protocol 2
sends the key through the private stdin pipe, with redacted, zeroizing key
buffers, and writes only encrypted graph/checkpoint bytes.

Candidate vectors have a 16 MiB admission budget, including a conservative
per-entry allocation allowance. This is a cache accounting budget, not an RSS
guarantee: in-flight guards can retain allocations after eviction. Resident
exact-search matrices keep the existing 32 MiB semantic and 128 MiB CLIP budgets.
Larger exact searches remain paged. Graph ciphertext admission is capped at
512 MiB; decrypting a graph temporarily needs both ciphertext and plaintext
buffers, and its retained plaintext occupies private memory.

UI locking and policy changes clear derived read caches and disarm the ANN
reader. Queries bind the admitted UI session before loading/scoring, check it
again before returning, and reject old snapshots after locking or a later
unlock. Matrices, cached vectors, query vectors and graph buffers zeroize when
their final owners drop. Fresh write keys remain available for locked capture.
Broker-derived key access is revalidated per key within one second, rather than
remaining authorized solely by a cached "service enabled" status.

The existing 60-second timer releases idle matrices/ANN readers after five
minutes. Memory pressure evicts candidate/tail caches. A pinned query keeps its
backing graph alive until it finishes, with the USearch view dropped before its
decrypted buffer. Clearing application buffers cannot erase plaintext already
copied into the Windows pagefile, a crash dump or another process.

### Synthetic measurement

Run the isolated benchmark without opening CarbonPaper or any user database:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark-storage-encryption.ps1
```

It imports the actual `ann_format.rs` and `payload.rs`, using normalized synthetic
vectors and an explicit public fixture key. Measurements below use a Ryzen 7
8845H, Rust 1.97.1, release optimization, one thread and five samples on
2026-11-03, with dependency versions matched to the application's lockfile. The
OS file cache is not flushed. "Cold" refers to the application
decryption cache; hot candidate results divide a 100-batch measurement by 100.

| Operation, median | 384 dimensions | 512 dimensions |
| --- | ---: | ---: |
| Scan 100,000 plaintext mmap vectors | 8.79 ms | 8.68 ms |
| Authenticate/decrypt and scan 100,000 vectors, uncached | 306.52 ms | 369.35 ms |
| Rescore 512 plaintext mmap candidates | 0.027 ms | 0.038 ms |
| Rescore 512 encrypted candidates, decryption cache cold | 1.815 ms | 2.387 ms |
| Rescore 512 encrypted candidates, cache hot | 0.043 ms | 0.068 ms |
| Decrypt a synthetic 32 MiB graph-size payload | 42.15 ms | 41.24 ms |

The envelope adds 52 bytes per vector: 3.39% at 384 dimensions and 2.54% at
512 dimensions. File size also includes headers, subject IDs, offset tables and
alignment. The first full scan costs substantially more than reading plaintext;
caching keeps that linear decrypt cost out of repeated scoring. These numbers
exclude CNG, SQLCipher, USearch traversal, model inference, disk-cold startup and
the frontend, and do not establish end-to-end query latency or startup time.

Raw samples: [storage-encryption-domains.json](benchmarks/storage-encryption-domains.json).
Tool: [benchmark crate](../tools/storage-encryption-bench/Cargo.toml).

## Existing datasets and operational limits

Initialization seals legacy derived fields in bounded, restartable transactions.
Process-token conversion preserves screenshot processing and recap revisions by
temporarily replacing the relevant triggers inside each page transaction.
`anchors.json` is imported into encrypted storage and removed after the import
commits; a failed import retains the source. A reappearing legacy file cannot
replace committed encrypted anchors and is retired at startup.

Old flat files, graph files and partial construction caches are rebuildable and
are removed. Failure to retire a legacy plaintext cache stops migration. Startup
cleanup subsequently preserves files referenced by active encrypted builds,
including frozen inputs, complete/previous checkpoints and partial copies.

Migration checks free disk space, enables secure deletion, truncates the WAL,
runs `VACUUM` and checks/truncates the WAL again. The version marker is written
only after compaction succeeds. Initialization and capture stay blocked until
this one-time work finishes. Interrupted work resumes without discarding
summaries or previously sealed fields. Older app/worker versions do not
understand the new inner/sidecar formats and should not write a migrated dataset.

The public bootstrap still exposes timestamps, IDs, relationships, classification
categories (including custom category labels), hashes/fingerprints, model and
dimension metadata, revisions and operational status. Known inputs may be
matched against surviving hashes. This patch protects the listed derived
content; it does not authenticate every bootstrap row or make all metadata
confidential. Migration cannot retroactively protect older backups or guarantee
forensic erasure of deleted SSD blocks and historical file copies.

## Validation

- Main Rust library: 990 passed, 13 existing ignored tests.
- App-bound library: 52 passed, one administrator-only temporary-service test
  ignored. The new domain-grant regression uses isolated native pipes and real
  impersonation/DPAPI, and reopens its temporary ledger.
- Both crates pass `cargo check --locked --all-targets` with their respective
  `--manifest-path src-tauri/Cargo.toml` and
  `--manifest-path src-tauri/app-bound/Cargo.toml` arguments.
- Worker/binary tests: 40 passed. `npm run test:security`, `npm run lint:ci`,
  `npm run build` and `npm run i18n:check` passed. Native runtime/debug script
  regressions passed (14 tests); the real application was not started.
- Native storage/credential tests use unnamed ephemeral CNG keys and temporary
  datasets. They cover public-key substitution, missing-key failure, public-only
  DB inspection, interrupted migration, cross-record tampering, dataset reset,
  locked/reunlocked query rejection and cache/backing-buffer lifetime.
- Release installation, real-user migration and end-to-end startup/query
  performance require separate acceptance runs; the synthetic benchmark does
  not exercise them.
