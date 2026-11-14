# App Depot Server architecture

This document defines the current product contract. App Depot Server is a self-hosted service for storing, cataloging, publishing and distributing arbitrary software artifacts. Publication requires only an App name and one file or a directory tree containing at least one regular file. Software-specific declarations are optional. The Server need not determine whether content is executable and never executes uploaded payloads. Installation, execution, runtime management and device integration belong to independent clients.

## 1. Entities and identities

An **App** has a stable server-generated UUID, a human-readable name, a separate mutable slug, an owner, visibility, optional declarations and Draft or Published state. Changing its name or slug does not change its UUID. A freed slug may be reused.

A **Release** belongs to an App and has a stable internal ID, an optional user-visible version and optional declarations. For a minimal upload, the Server creates a Release as an implicit technical revision: users need not manually create or name it. A Published Release contains at least one Artifact.

An **Artifact** is one immutable file or directory tree with a content identity. An Artifact tree contains at least one regular file. The same content may be used by multiple Releases. A **Release–Artifact association** attaches an Artifact to one Release and owns Artifact-specific declarations in that Release's context. Identical bytes may have different declarations in different Releases; shared byte storage does not merge mutable metadata.

A **CAS object** is an immutable content-addressed storage object. Regular files are verified with SHA-256; verified bytes may be reused across legitimate references. App UUID, Release ID, Artifact content identity, CAS object identity and declaration revision ID have distinct meanings and purposes. Matching digest strings do not make them interchangeable. Name, slug, visible version and declaration revisions do not determine content identity.

## 2. File tree contract

A file Artifact's identity is the SHA-256 of its bytes. A directory Artifact's identity is SHA-256 of its versioned canonical binary tree representation, independent of its source root name, absolute path, ingestion method, App, Release and declarations. The exact format, validity rules, ZIP mapping and test vectors are specified in [Canonical Tree Identity v1](tree-identity.md).

The portable representation uses `/` to separate components and preserves their Unicode sequences without case conversion or normalization. Invalid paths and unsupported filesystem entries are rejected. The executable flag is the only portable permission attribute. Empty nested directories are represented explicitly; a tree must contain a regular file. A valid tree may be impossible to materialize on a particular client filesystem; the client checks destination compatibility before writing it.

## 3. Upload and publication

**Upload** transfers and verifies content; it does not publish it. **Registration** binds an App, Release, Artifact and verified CAS references but does not automatically publish the Release either. **Draft** permits editing; a Draft App may have no payload. **Publication** is an explicit operation making a complete Release available subject to visibility and access policy. **Visibility** governs access to metadata and content rather than Release state: Published may remain Private.

The minimal user flow is to provide an App name, select a file or directory, wait for receipt and verification, then publish immediately or continue in Draft. The Server creates the App, implicit Release and Artifact association without requiring a version, platform, entrypoint, installer, manifest, category or other software metadata. Draft Release composition may change. Publication freezes composition; changing a Published Release's Artifact composition requires a new Release. Editing declarations does not. If the last Published Release is deleted, the App returns to Draft.

Channels are optional. Each channel is a named mutable reference from an App to one of its Published Releases; there are no mandatory built-in channels. Moving a channel changes neither the Release, Artifact content nor access rights. Movement history is retained for the lifetime of the App, except records that must be removed with a deleted Release or App.

## 4. Declarations

Declarations may belong to an App, Release or Release–Artifact association. All software-specific declarations are optional; declaration data may be any valid JSON value, including `null`. Server-managed fields are separate from user declarations. User data cannot override identities, owner, access control, visibility, lifecycle state or current-revision references. Standardized declarations and extension fields may coexist; a collision with a reserved Server field is invalid.

Possible declarations include names and descriptions, platforms and architectures, version information, runtime requirements, entrypoints, installation hints, dependencies, user-data locations, licenses, artwork, categories and tags. None is mandatory or defines a universal installation or execution contract. The Server stores declarations without executing them. A client interprets only declarations it understands and acts only with the necessary authority. Unknown fields may be stored and returned, but do not by themselves make executable instructions trusted.

## 5. Import and source of truth

Declarations may arrive through embedded manifests, separate documents, web administration or API requests. Embedded manifests are optional. Sources are not automatically merged and have no implicit precedence: the user selects a source or explicitly resolves conflicting input. Upload without declarations remains possible.

Once incorporated, Server-stored declarations are authoritative for App Depot clients. Users can correct and supplement them through the Server without changing payload bytes. Original embedded metadata remains distinguishable from current Server declarations. The Server does not silently rewrite embedded manifests.

## 6. Revision history and consistent reads

App, Release and each Release–Artifact association have independent declaration histories. Each saved change creates a revision; older revisions remain available while their owner exists, and the current revision is identifiable. Restoring previous metadata creates a new revision. Concurrent edits do not silently overwrite one another. Clients detect declaration changes independently of content identity. A declaration revision creates neither a new Artifact nor a new Release and changes neither bytes nor Published Release composition.

When the API returns combined declarations from multiple levels, the response represents a logically consistent snapshot. Permanent deletion of an owner removes its revision history; an association's history belongs to the association, not to shared CAS bytes.

## 7. Ownership, visibility and access

An App has an owner; the MVP has no Publisher organization entity. Visibility defaults to Private. Public access requires explicit configuration. The MVP supports local user accounts and personal API tokens without requiring an external identity provider. Authorization applies to metadata and content, including CAS-addressed reads: knowing a digest does not grant access to a private Artifact. Authentication protocol details and role hierarchy are not fixed here.

A global Server setting controls anonymous reads. When enabled, anonymous clients may read only explicitly Public resources that are Published and otherwise eligible for access. When disabled, anonymous reading is unavailable even for Public resources. Private resources remain protected in either case. Anonymous clients cannot upload, edit, publish, administer or delete content. A public API is a documented client-facing contract, not unrestricted anonymous access.

## 8. Permanent deletion

Explicit deletion permanently removes the selected user-managed object and its exclusively owned dependent data from active App Depot-managed state. The MVP has no trash, soft delete, grace period or user-facing recovery. Deletion immediately revokes access to the target, removes catalog records and associations, owned declarations and all their revision history, applicable channel references and history, artwork and other metadata, related temporary uploads and other dependent state. An Artifact record and metadata are removed when no surviving Release references it. A CAS object is deleted when no legitimate references remain; shared objects remain until their last such reference is removed.

Deletion is an observable operation with status. It recovers after interruption and leaves no permanently orphaned managed data. Acceptance of permanent App or Release deletion atomically revokes access before the API returns `202 Accepted`. Access revocation and physical cleanup are distinct. The API reports access revocation and incomplete physical cleanup separately; cleanup failures remain visible and can be retried without a user-facing trash mechanism. Success is reported only after all required managed-state and storage cleanup completes.

Server deletion does not erase offline-client copies, previously downloaded files, independent backups, ZFS snapshots or data retained by storage hardware. It does not guarantee physical secure erasure or give the Server authority to remove files from client devices. Client-side deletion belongs to the corresponding client. Shared-reference limits and cleanup failures are reported explicitly.

## 9. Storage and deployment

The single-node MVP uses SQLite for metadata and filesystem-backed CAS with SHA-256 verification. One OCI container runs API, Web UI and workers on the official M8 `linux/amd64` target. Docker Compose is the reference deployment; TrueNAS SCALE Custom App uses the same contract. One persistent data root, non-root operation, online backup and offline restore follow the [deployment and operations contract](deployment-operations.md). Specific deployment files are not defined here.

## 10. Public API

The API follows REST over HTTP at `/api/v1`. Structured resources use JSON; binary transfers use HTTP bodies. [API v1](api.md) defines routes and semantics, and [OpenAPI 3.1](openapi.yaml) defines machine-readable resources. The capabilities endpoint reports actual support. Catalog, management, publication and content transfer are available to independent clients without a Web UI; readers need no management privilege.

The catalog supports App, Release and Artifact discovery, optional channel resolution, and access to current declarations and their revision history. Separate resources exist for each applicable declaration level; a combined view across levels remains a logically consistent snapshot. Declaration changes use strong ETag and If-Match; conflicting concurrent edits are rejected while revision IDs retain their own meaning.

## 11. Content ingestion modes

**Bare ingestion** accepts one file or a directory tree and preserves original file bytes, relative path names, directory structure and supported portable attributes. Physical storage may use CAS instead of reproducing the original layout. An ordinary ZIP remains an opaque file Artifact unless the user explicitly selects archive-content import.

**Transport-archive import** is explicitly selected. The MVP supports only ZIP with Store and Deflate compression methods. The publisher creates the archive; the Server does not create publisher upload bundles, and bundle creation tools and packaging policy are outside its responsibilities. The Server receives the complete ZIP in temporary storage, verifies the archive, safely parses and validates its contents, registers the resulting file tree in CAS, then removes the temporary ZIP after successful registration. The transport ZIP is not a mandatory App Depot application format. Embedded declarations are not required; ordinary ZIP upload never switches to import automatically.

Equivalent relative paths, file bytes, directory structure and executable flags yield the same canonical tree identity through bare ingestion and ZIP import. The ingestion method is absent from Artifact content identity.

## 12. Upload sessions, integrity and registration

Upload uses resumable sessions and bounded memory even for very large files. ZIP transport resumes by byte range without understanding internal archive file boundaries. For trees, an inventory is sent first and then missing files; previously verified content is not needlessly retransmitted. Concurrency adapts to load and is bounded by Server resources. Chunk sizes, range bookkeeping, retry algorithms and exact concurrency limits are not fixed here.

The MVP deduplicates whole files in CAS. The Server independently verifies received bytes before trusting their identity; client-supplied hashes are claims, not proof. A previously verified CAS object can be reused without a mandatory full rehash on every access; it is reverified when necessary. Optional integrity audits do not block every download. Unnecessary reads, copies and hashing passes are avoided without weakening verification. Sequential transfer may allow hashing during receipt; resumed or out-of-order transfer may require another pass.

An Artifact becomes available only after complete receipt, integrity and required import validation, safe CAS commit and consistent catalog registration. Partial uploads and incomplete imports do not appear as complete published Artifacts. Acknowledged upload progress survives process restart but may be invalidated after power loss. `Completed` requires durable CAS content and SQLite registration. Recovery, finalization and garbage collection follow the [reliable upload and commit contract](upload-commit.md).

ZIP import applies configurable storage quotas, free-space checks, bounded memory and concurrent work, and protection against unsafe paths and resource exhaustion. Malformed and unsupported ZIP structures are rejected, and extracted files are validated before registration. Configurable quotas do not disable essential safety checks; arbitrarily small fixed limits must not exclude legitimate large imports. Peak temporary capacity accounts for the complete transport ZIP and generated objects. Temporary data is not registered CAS content. Interrupted or failed imports can be recovered or cleaned without permanent orphaned data; zero-copy or guaranteed single-pass processing is not assumed.

## 13. Content download

Clients receive a logical Artifact representation independent of internal CAS layout. A directory provides a file inventory and access to each file's content with HTTP Range. Clients can resume downloads, reuse already verified local files, and construct the final directory directly during transfer without downloading and extracting a complete archive. Archive export is outside the MVP download contract. Internal CAS paths and physical object addresses do not become public API dependencies. Every download is subject to authorization. Application installation and runtime preparation remain client responsibilities.

## 14. Local-network transfer priorities

App Depot is LAN-first. The MVP targets large individual files, including archives around 100–200 GB or larger, and trees with many files of varied sizes. The goal is high effective throughput with bounded memory, efficient network use, and no unnecessary disk reads, copies or hashing. Evaluation spans source reads, network transfer, temporary writes, hashing, CAS writes, metadata operations and finalization rather than network speed alone.

Transfer parameters and adaptive parallelism are tunable based on benchmarks. One sequential transfer can outperform multiple ranges for a large file; parallelism may help for many small files. Local-network transport may have a dedicated performance profile, but retains the same API, access control, integrity verification, domain model and content identity. WAN optimization belongs to later work. Unverified numerical performance targets are not fixed here.

## 15. Implementation structure

The Server is a Rust modular monolith with domain, application, infrastructure and HTTP Server boundaries. SQLite coordinates catalog state and durable operations; the filesystem stores CAS objects and temporary transfer data. Local Dev/Preview workflows use the real Server, SQLite, CAS and API. This structure implements the contracts above without changing public API, identity, authorization or durability rules.

## 16. Future application portability

Future work may export one App, a selected Release and its original Artifacts, current artwork and current App, Release and association declarations as a self-contained logical package. A possible layout has `original/`, `artwork/`, `declarations/` and an integrity and relationship manifest; this is a direction, not a package-format specification. Metadata and artwork work must preserve immutable content-addressed original Artifacts, independently identifiable content-addressed artwork with explicit current references, and portable declarations that do not depend on physical Server paths. The App, Release, Artifact and association remain distinct entities; portable metadata must not depend on local absolute paths, host configuration or Server database IDs. A unified logical package does not require physically duplicating entire App directories. Export, import, snapshot generation and package validation are outside the current scope.
