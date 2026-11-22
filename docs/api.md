# HTTP API v1

The normative machine-readable contract is [OpenAPI 3.1](openapi.yaml). This document defines cross-request semantics. The base path is `/api/v1`; JSON resources use `application/json`, binary transfer uses `application/octet-stream`, and errors use RFC 9457 `application/problem+json`. Resource keys are stable IDs. A file Artifact ID is `adfile:sha256:<64 lowercase hex>`; a tree Artifact ID is `adtree:v1:sha256:<64 lowercase hex>`. App and Release UUIDs, association UUIDs, declaration revision UUIDs, upload/session/operation UUIDs and internal CAS identities are distinct. Slugs and visible versions are attributes, not keys. [Tree Identity v1](tree-identity.md) and [reliable upload/commit](upload-commit.md) remain authoritative.

Artwork content is the exception to the ordinary binary `application/octet-stream` response: it uses the validated image MIME type so browsers can display it safely.

## Resources and routes

| Area | Routes (relative to `/api/v1`) |
| --- | --- |
| Capabilities/account | `GET /capabilities`; `GET, POST /setup`; `POST /auth/sessions`, `GET, DELETE /auth/sessions/current`; `GET, PUT, DELETE /me/token`; `GET, POST /users`, `GET, PATCH /users/{user_id}`, `DELETE /users/{user_id}/token`; `GET, PATCH /settings` |
| Apps and Releases | `GET, POST /apps`; `GET, PATCH, DELETE /apps/{app_id}`; `GET, POST /apps/{app_id}/releases`; `GET, PATCH, DELETE /releases/{release_id}`; `POST /releases/{release_id}/publish` |
| Artwork | `GET /apps/{app_id}/artwork`; `GET, PUT, DELETE /apps/{app_id}/artwork/{role}`; `GET /apps/{app_id}/artwork/{role}/content` |
| Channels | `GET /apps/{app_id}/channels`; `GET, PUT, DELETE /apps/{app_id}/channels/{name}`; `GET /apps/{app_id}/channels/{name}/history` |
| Artifact context | `GET /artifacts/{artifact_id}`; `GET, POST /releases/{release_id}/artifacts`; `GET, DELETE /associations/{association_id}` |
| Declarations | `GET, PUT /apps/{app_id}/declarations`, `/releases/{release_id}/declarations`, `/associations/{association_id}/declarations`; each has `GET …/revisions` and `GET …/revisions/{revision_id}`; `GET /releases/{release_id}/declaration-snapshot` |
| Upload | `POST /uploads`; `GET, DELETE /uploads/{session_id}`; `GET, POST /uploads/{session_id}/inventory`; `POST /uploads/{session_id}/inventory/complete`; `PUT /uploads/{session_id}/files/{file_id}`; `GET /uploads/{session_id}/files/{file_id}/progress`; `POST /uploads/{session_id}/finalize`; `GET /uploads/{session_id}/result` |
| Server storage import | `GET /server-imports/roots`; `GET /server-imports/roots/{id}/entries`; `POST, DELETE /server-imports`; `POST /server-imports/gog/analysis`; Admin `GET /admin/import-sources` |
| Download/operations | `GET /artifacts/{artifact_id}/content`, `/inventory`, `/files/{file_id}`; `GET /operations/{operation_id}` |
| Operations and backup | `GET /health/live`, `/health/ready`; `POST, GET /backups`; `GET /backups/{backup_id}`, `/backups/{backup_id}/entries/{entry_id}`; `GET /admin/diagnostics` |

`Artifact` is immutable content; `Association` gives it Release-specific context. A Draft Standard Release may receive multiple associations, one at a time, via `POST /releases/{release_id}/artifacts`, and lose them via `DELETE /associations/{association_id}`. The existing Artifact must be legitimately accessible for association. Published Release composition is immutable; attempts return `409 immutable_release`. A Standard Release can have multiple Artifacts. `POST /releases/{release_id}/publish` alone publishes a Draft Release, requires the owner's current Publish permission (or Server Admin) and a strong Release `If-Match`. A Standard Release needs at least one associated Artifact. A Derived Release needs a resolvable recipe. Registration never publishes. An authorized minimal workflow may create a Draft App by `POST /apps`, then a Draft Release by `POST /apps/{app_id}/releases`, or bind an upload to `newAppName` / `appId` plus `createRelease:true` for atomic implicit creation at finalization. A bare tree still requires a regular file; no software declaration, version, channel or installer is mandatory.

## Derived Apps and recipes

Create a Derived App by sending `{"name":"Example Edition","kind":"derived"}` to `POST /apps`. Omit `kind` or send `standard` for the existing Standard workflow. App reads and lists return `kind`. PATCH cannot change it. Browse published targets using `GET /apps/{app_id}/releases?order=published` for descending Server publication order. Other release lists keep their existing ID ordering. The capabilities response advertises `recipeVersions: [1]`.

| Method and route | Behavior |
| --- | --- |
| `GET /releases/{release_id}/recipe` | Reads the definition and strong Release ETag. An unconfigured Draft returns JSON null to its manager. A non-manager needs access to the Published Release and its dependencies on every target. |
| `PUT /releases/{release_id}/recipe` | Replaces the Draft definition with a valid RecipeV1 JSON object. Requires management permission and the strong Release `If-Match`. Returns the normalized definition and updated validator. |
| `POST /releases/{release_id}/recipe/preview?platform=…` | Manager-only unsaved layout preview for a Draft. Accepts the same definition shape, with a syntactically valid placeholder launch path where needed. Returns sorted file and directory paths and eligible launch candidates without checking the root launch target's existence. |
| `GET /releases/{release_id}/recipe/lock?platform=…` | Resolves one target of the saved definition against an authorized catalog snapshot. Managers can validate Drafts. Returns a `RecipeLockV1`. |
| `POST /releases/{release_id}/publish` | Uses existing publication headers and permissions. Revalidates all targets of a Derived Recipe within the publication transaction. |

Cookie-authenticated mutations and previews use the existing same-origin and CSRF rules. Recipe routes use `Cache-Control: no-store`. Standard Releases and inaccessible roots return `404 not_found` on Recipe routes. Missing and stale write validators use `428 precondition_required` and `412 stale_etag`. Writes to a Published recipe return `409 immutable_release`. A Derived Release accepts neither Artifact upload nor association creation.

Recipe problems return 422 with an RFC 9457 code. `recipe_required`, `invalid_recipe` and `unsupported_recipe_version` cover definition shape and version. `dependency_unavailable`, `dependency_cycle`, `dependency_conflict` and `platform_mismatch` cover selection and authorization. `recipe_association`, `recipe_source`, `recipe_collision`, `recipe_launch` and `recipe_limit` describe assembly failures. Missing, deleted and inaccessible dependencies share the same safe code. See the [RecipeV1 contract](recipe-v1.md) for processing and lock guarantees.

## App artwork

Artwork is optional and App-owned. It is independent of Releases, software Artifacts, associations, declarations and App kind. Current roles include `icon`, canonical `logo`, `steam.grid.vertical`, `steam.grid.horizontal`, `steam.hero` and the older `steam.logo`; other lowercase role identifiers are accepted. A role has at most one current image per App. Role identifiers are 1–80 ASCII characters, start with a lowercase letter, use lowercase letters, digits, `.`, `_` or `-`, contain no `..`, and end with a letter or digit. Clients percent-encode the role as one URL path segment. New Steam-compatible consumers use `logo` first and may fall back to `steam.logo`. A read of `steam.logo` resolves `logo` only when no explicit `steam.logo` binding exists; the returned metadata identifies its actual role. Writes and deletion always target the explicit role, and listing returns both bindings when both exist. There is no automatic migration or deletion of older bindings.

`GET /apps/{app_id}/artwork` returns `{ "items": [...] }` ordered by role. `GET /apps/{app_id}/artwork/{role}` returns current metadata and a strong binding ETag. Metadata contains `appId`, actual `role`, opaque binding `id`, SHA-256 `digest`, detected `mimeType`, stored `size`, `width`, `height` and `etag`. The digest is an immutable content identity, not an access credential or software Artifact ID. `GET …/content` returns stored image bytes with the detected safe image MIME type, `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`. PNG, JPEG and WebP preserve uploaded bytes; ICO input is normalized to PNG, so the digest, size and downloaded bytes describe the PNG. An App owner or Server Admin may read and manage artwork on a Private or Draft App; other readers may read it only when the App is Public and Published, and anonymous readers additionally require the global anonymous-read policy. Knowledge of a digest does not grant access. A missing or inaccessible binding returns `404`.

`PUT /apps/{app_id}/artwork/{role}` accepts raw PNG, JPEG or WebP bytes, plus ICO for `icon` only, and requires an `Idempotency-Key`; browser sessions also require Origin and CSRF checks. Creation requires `If-None-Match: *`; replacement requires the current strong `If-Match`. The Server detects and fully decodes the image rather than trusting the supplied MIME or extension. ICO directory entries are bounded and checked before decoding; the selected embedded image is converted to RGBA PNG, preserving alpha. The maximum input and stored image size is 8 MiB, each dimension is at most 8192 pixels (1024 for ICO output), and decoded area at most 16 megapixels; invalid or unsupported images return `422`, oversized input `413`. A successful creation or replacement returns `200` with current metadata and a new strong ETag. Exact idempotent retry returns the prior successful result; conflicting key reuse returns `409`. Replacement remains invisible until verified CAS bytes and the new binding commit. `DELETE` requires the current strong `If-Match` and returns `204`; it removes only this App-role binding. Missing preconditions return `428`, stale ones `412`. Unreferenced bytes are reclaimed after cleanup; shared CAS bytes remain while another legitimate reference exists. Artwork is included in online backup and offline restore.

## Server-storage import

Server-storage import has a built-in `inbox` root when the sibling Inbox directory is available. The Server creates it at startup when permitted; its browsing and import are Server Admin-only. `GET /server-imports/roots` lists only allowed logical IDs, display names and source kinds. `GET /server-imports/roots/{id}/entries?path=&pageSize=&cursor=` pages regular files and directories below one allowed root. Empty `path` means the root. Neither endpoint exposes host paths or symlink targets. Server Admins may access configured roots; other users require their stable user ID in that root's explicit allowlist. Starting an import additionally requires the ordinary upload permission for the chosen new App, existing App or Draft Release target. Private App and publication authorization remain unchanged. Admin-only `GET /admin/import-sources` reports source IDs, kinds, availability, container paths and read-only import behavior; it does not reveal private content or modify host configuration.

`POST /server-imports` takes `{rootId,path,mode,target}` with `mode` equal to `BareFile`, `BareTree` or `ZipImport`. `BareFile` treats a ZIP as an opaque File Artifact; `ZipImport` explicitly imports its content as a Tree Artifact. The request requires an `Idempotency-Key`; cookie authentication also requires Origin and CSRF validation. A successful response is `text/event-stream`: each `data:` frame is JSON with `phase`, optional `sessionId`, file/byte progress while receiving, and a terminal `result` or safe `problem`. `Scanning`, `Verifying`, `Importing` and `Committing` have no fabricated percentage. The imported upload session and finalization operation use the existing `/uploads/{id}` and `/operations/{id}` behavior. Registration creates a Draft Release and never publishes automatically. The Server checks source size and modification time around scanning and transfer, then independently verifies the staged SHA-256; a changed source is rejected. An exact retry with the same key and request resumes safe ranges after interruption; changing source bytes before completion requires a new intentional key.

`DELETE /server-imports` with `{requestKey}` and normal mutation authentication requests cancellation even during scanning. If an upload session already exists, this uses the existing cancellation operation; durable registration may win the race and remain `Completed`. The source is never deleted or modified.

### GOG Offline Installer

`POST /server-imports/gog/analysis` accepts `{rootId,path,setup?}`. The selected source is a directory in an authorized import root. Empty `path` selects the root. The response is `{candidates,analysis,analysisId}`. If there are multiple regular setup executable candidates and no explicit choice, both analysis fields are null. Repeat with the chosen basename. On success the opaque `analysisId` binds the inspected bytes to the principal, logical root and directory. Use the usual authentication and CSRF conventions. The response is marked `Cache-Control: no-store`.

Submit `POST /server-imports` with `mode:"GogOfflineInstaller"`, the usual `rootId`, `path` and `target`, and `gogReview:{setup,analysisId,hints?}`. Review hints are `productName`, `gameVersion`, `candidateExecutable` and `notes`. The stream adds `ExtractingInstaller` before ordinary inventory-backed transfer. Keep the idempotency key to recover interrupted work. Use `DELETE /server-imports` with the same request key to cancel an active import. Abort an analysis request to cancel its worker. Successful import includes the existing FinalizationResult and writes the installation manifest to the association's initial declaration revision.

Analysis returns `422` for `invalid_gog_source`, `gog_missing_volume`, `gog_invalid_volume`, `gog_unsupported_format` and `gog_limit`, and `503 gog_unavailable` when the backend or isolation is unavailable. A busy worker returns `409 operation_running`. Extraction uses the same codes in its terminal SSE problem and uses `source_changed` when the review no longer matches. Publication uses the usual separate Draft publication request. See the [versioned contract](gog-offline-import.md).

## Authentication and authorization

Local accounts authenticate API calls using `Authorization: Bearer <personal-token>`. Each user has at most one active personal token, with no mandatory expiry. `PUT /me/token` issues or replaces it and reveals the secret only in that response; the prior token is invalid immediately. `GET /me/token` returns only metadata; `DELETE` revokes. Admin may revoke another user's token through `DELETE /users/{user_id}/token` without seeing its secret. The Server stores no plaintext token and excludes it from logs. On a fresh data root, `GET /setup` returns `{ "required": true }`. `POST /setup` requires the startup-log token, a same-origin request, a unique username and a password of 16–1024 characters; it creates exactly one first Admin and returns `201 { "created": true }`. Failed verification or completed setup returns `403`; no token is returned by HTTP. The startup token rotates on restart until setup succeeds. Web UI calls then use a separate login session: `POST /auth/sessions` requires a same-origin check and returns an `ad_session` cookie with `HttpOnly`, `SameSite=Lax`, `Path=/` and no `Domain`, plus a CSRF token. The cookie has `Secure` when the effective origin is HTTPS. Cookie-authenticated state-changing calls send `X-CSRF-Token` and pass Origin checks. Logout invalidates the session. Bearer requests do not use CSRF tokens. Only configured proxy peers may supply HTTPS forwarding; untrusted forwarding headers are ignored. No OAuth/OIDC dependency is required.

| Action | Anonymous | Authenticated reader | App owner | Owner with Publish | Server Admin |
| --- | --- | --- | --- | --- | --- |
| Read Public Published catalog/content | Only if global anonymous reading enabled | Yes | Yes | Yes | Yes |
| Read/manage Private App, Draft content and declarations | No | Only own resources | Own resources | Own resources | All resources |
| Create App | No | No | No without Publish | Yes | Yes |
| Manage owned App/Draft Releases, associations, uploads, declarations; delete owned App | No | Own resources | Yes | Yes | All resources |
| Publish owned Draft Release | No | No | No without Publish | Yes | Yes |
| Users, Publish grants and global settings | No | No | No | No | Yes |
| Backup creation/retrieval and operational diagnostics | No | No | No | No | Yes |

“Reader” is an access level, not a required assigned role. Ownership persists when Publish is revoked; ordinary management remains allowed and existing Published Releases stay published. Admin may manage another user's Private App. Public visibility alone does not expose Draft content. Catalog lists/search, direct IDs, Artifact IDs, deduplication negotiation and operation status all apply the same access filter. An inaccessible Private resource returns `404` without confirming existence. Missing authentication for a protected action returns `401`; a forbidden action on a resource the caller may see returns `403`. Possessing a digest never grants CAS read or reuse; `/artifacts/{artifact_id}` and download routes resolve authorization through an accessible association, including Public Published context when applicable. Physical CAS paths are never returned.

## Concurrency, declarations and pagination

Mutable App, Release, association, channel, settings and current declaration representations have strong ETags. `PATCH`, declaration `PUT`, conditional deletion and Publish require `If-Match`; omission gives `428`, a stale validator `412`. Channel creation requires `If-None-Match: *` and returns `201`; movement requires current strong `If-Match` and returns `200`; exactly one of these headers is supplied. A channel name is one nonempty URL path segment and cannot contain `/`; clients percent-encode it. A channel refers only to a Published Release in its App. Channel movement history is paginated and retained under the architecture's lifetime rules. No built-in channel is required.

App, Release and association declaration documents are independent. Each starts with an empty `data` object and a current revision. `PUT` replaces the entire `data` value; it must be present and may be any valid JSON value (object, array, string, number, boolean or `null`). A present `data: null` is valid, while an absent `data` is invalid. Values are neither implicitly merged nor interpreted. The optional `data.appDepot` metadata and entry-point convention, including [Native portable launch v1](architecture.md#native-portable-launch-v1), does not restrict accepted JSON values. `POST /associations/{id}/entry-points/validate` accepts `{ "data": <any JSON> }` and returns a read-only assessment against that association's Artifact: existing overall target `status` is `Absent`, `Valid`, `Issues` or `Unsupported`, with indexed per-point target `status` of `Valid`, `Missing`, `NotFile`, `Invalid` or `Unsupported`. The response additionally reports overall `launchStatus` (`Absent`, `Valid`, `Issues` or `Unsupported`), per-point `launchStatus` (`Absent`, `Valid`, `Invalid`, `Unsupported` or `TargetUnavailable`), optional machine-readable `launchReason`, known `effectivePlatforms`, and `selectionRequiredFor` when more than one valid point in this association applies to a known platform. Target statuses retain their old meaning: a `Valid` target alone does not declare launchability. The assessment uses current Release platforms where present, follows association read authorization, creates no revision and does not decide local execution readiness. Structured clients should assess what they edit before saving; raw declaration PUT remains lossless for unsupported or unresolved data. Every accepted edit, including restoration of old data, creates a new revision. Server fields stay outside `data` and cannot be overridden. Current documents and revision history have separate routes. The combined Release snapshot reads one consistent App, Release and association declaration state and has its own strong ETag; that validator is neither a declaration revision nor a content identity. Historical revisions are immutable. A declaration edit does not change Artifact bytes or Published Release composition.

The following are **normative declaration examples**. Each JSON value is an Association declaration `data`; the same schema is independent of the client language or UI. The Association already identifies one Artifact. Release `data.appDepot.platforms`, if present, further constrains the known platform set. A File target is valid only for a File Artifact, and a Tree target path must exist as a regular file in that Artifact's immutable inventory.

```json
{"appDepot":{"label":"Source archive"}}
```

This is downloadable data without a launch. The next two independent Association examples target the Linux x86_64 and ARM64 files of a Tree Artifact. The explicit working directory may be omitted because the default is the target's parent (`System64` or `SystemARM64`), but it is shown to demonstrate the syntax. File names here are publisher choices, not detection rules.

```json
{"appDepot":{"entryPoints":[{"id":"game-x64","target":{"type":"TreeFile","path":"System64/unreal-bin-amd64"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"System64","arguments":[]}}]}}
```

```json
{"appDepot":{"entryPoints":[{"id":"game-arm64","target":{"type":"TreeFile","path":"SystemARM64/game"},"platforms":["linux/aarch64"],"launch":{"method":"NativePortableV1","workingDirectory":"SystemARM64"}}]}}
```

A portable single-file Artifact uses `{"id":"tool","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","arguments":["--safe-mode",""]}}` in `entryPoints`; its working directory defaults to the local Artifact root. The empty second argument is preserved. Two points with the same applicable platform remain two choices, even when they have the same target: `entryPoints` containing IDs `client` and `server`, both with `platforms:["linux/x86_64"]` and valid `NativePortableV1` launches, reports `selectionRequiredFor:["linux/x86_64"]`; a client MUST request explicit selection and MUST NOT choose array position zero. The same rule applies across associations, which this per-association endpoint cannot enumerate as one choice set.

`{"id":"legacy","target":{"type":"ArtifactFile"}}` is a valid historical file reference but has `launchStatus:"Absent"`. `{"id":"future","target":{"type":"ArtifactFile"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV2"},"extension":{"keep":true}}` remains stored and downloadable, with launch `Unsupported`; the unknown extension is preserved and inert. `{"id":"runtime-note","target":{"type":"ArtifactFile"}}` with Release `runtime:{"name":"Example runtime"}` still has no executable semantics: runtime is descriptive. `{"id":"bad-dir","target":{"type":"TreeFile","path":"bin/game"},"platforms":["linux/x86_64"],"launch":{"method":"NativePortableV1","workingDirectory":"../outside"}}` has launch `Invalid` while its target can remain `Valid`; the declaration is still stored and downloadable. An unknown platform token such as `linux/amd64` has no v1 match. These examples do not imply that any client has installed dependencies or is authorized to execute content.

Collections use `pageSize` (default 100, protocol maximum 1000; the Server may advertise a lower configured maximum in capabilities) and opaque `cursor`; responses have `items` and `nextCursor` (`null` at end). Ordering is stable by resource ID unless explicitly requesting `order=published`. Artifact inventory ordering follows canonical UTF-8 path order. A page is internally consistent; changing catalog pages do not form a global snapshot. Cursors are bound to query, principal/scope and ordering; invalid or expired cursors return `400 invalid_cursor`, and authorization changes never reveal hidden items. Artifact inventory is immutable and its ETag and cursor refer to the same representation across pages. Upload inventory and range-map pages carry `inventoryVersion` or `progressVersion`; if it changes during traversal, the cursor is rejected with `409 progress_changed` and the client restarts. Large inventories, upload inventory/progress and revision history use this scheme; no binary inventory format is required. JSON size and offset integers may span uint64; clients must parse them exactly without floating-point rounding.

`Idempotency-Key` is required for catalog/upload resource-creating POST, explicit Publish, inventory append/complete, finalization and asynchronous DELETE; browser login is a separate security-session action. It binds principal, operation, target and immutable inputs; conflicting reuse returns `409 idempotency_conflict`. An exact authorized replay resolves the saved operation/result before checking whether its target still exists or its old `If-Match` still matches. Ordinary keys remain valid while work runs and for at least 24 hours after terminal outcome. Registration's stored finalization result remains available for the registration lifetime; an old session/key cannot recreate a deleted registration. Repeated or concurrent finalization resolves to one result. Strong ETags prevent lost updates but do not verify content bytes.

## Upload, download and operations

`POST /uploads` selects `BareFile`, `BareTree` or `ZipImport`. Its target is exactly one of `releaseId` (existing Draft Release), `newAppName` (create Draft App and implicit Release), or `appId` with `createRelease:true` (implicit Draft Release in owned App). Authorization and the target's Draft state are rechecked at durable finalization; a revoked Publish grant cannot be used for implicit App creation. BareFile and ZipImport bind the transport file's expected size and SHA-256; the returned `file.fileId` identifies it. BareTree is inventory-first: append validated batches at `POST /uploads/{id}/inventory`, then freeze with `/inventory/complete` before transferring any file. File entries have server-assigned opaque `fileId`; directory entries do not. For BareFile/BareTree, `transferRequired:false` may be returned only for a file backed by verified CAS content already legitimately accessible to this principal; omission means transfer is required. No response distinguishes inaccessible private content from absent content. Such a reused file needs no PUT; a range map describes received staging, not reused CAS bytes. ZIP transport always requires full receipt. Inventory entry names, file hashes and attributes follow Tree Identity v1. ZIP import is explicit, receives one whole transport ZIP by byte ranges, accepts Store/Deflate and never registers the ZIP as a mandatory separate Artifact.

`PUT /uploads/{session_id}/files/{file_id}` is an App Depot partial-PUT extension, requiring `Content-Range: bytes start-end/total` with inclusive bounds and exactly `end-start+1` body bytes. A zero-byte file needs no PUT and may finalize with an empty range map. Proxy chains must pass `Content-Range` and streaming bodies unchanged; they must not coalesce, rewrite or cache partial PUTs. `204` acknowledges staged bytes and recorded progress under F2.2, not full-file verification. `GET …/progress` reports sorted, disjoint half-open ranges `[start,end)`; `bytes 0-1023/4096` becomes `[0,1024)`. Only `Receiving` accepts bytes. After power loss, progress may shrink following actual-byte reconciliation. `POST …/finalize` returns `202` with an operation while work runs or `200` with a terminal result; `GET …/result` and `GET /operations/{id}` resolve lost responses. The session's `operationId` remains the one finalization operation even if a caller retries with another key; no second registration is created. `DELETE /uploads/{id}` requests cancellation before commit and returns the existing `Completed` result if commit won. `Completed` is returned only after durable CAS and SQLite registration, including the association and cleanup obligation. It never implies publication.

A terminal staged-file `hash_mismatch` result includes a Problem `detail` naming the original logical file path and the failed size or SHA-256 check. Only the authorized session owner or Server Admin can retrieve that result. The Server log records opaque identifiers and the verification phase, without the logical path, physical path or content digest. The failed session cannot be resumed; reselect and inspect changing source files before starting a new upload. Incomplete ranges remain a distinct `incomplete_upload` error and can be retried from authoritative progress while the session is `Receiving`.

After offline backup restore, an incomplete upload is a retained terminal `Cancelled` session with `cancellationReason: "RestoreInvalidated"`. Authorized `GET /uploads/{id}` returns `200` with that phase and reason; `GET /uploads/{id}/result` returns `200` with the same session/operation IDs, terminal `Cancelled` result and reason. The existing `/operations/{operationId}` reports `Failed` with `session_cancelled` and `cancellationReason: "RestoreInvalidated"`; no Completed registration is invented. `GET …/inventory`, `GET …/progress`, range `PUT`, inventory append/complete and `POST …/finalize` return `410 session_cancelled` with Problem Details extension `cancellationReason: "RestoreInvalidated"`; no stale ranges are advertised or accepted. An exact authorized retry of the original `POST /uploads` with its `Idempotency-Key` returns `201` and the same cancelled session representation, never a new session. A conflicting request under that key remains `409 idempotency_conflict`. The historical session/result and key binding remain queryable to its authorized principal for at least 24 hours after restore unless the target is permanently deleted earlier; ordinary terminal retirement may follow. Permanent target deletion removes dependent records under the strict deletion contract. After retirement or deletion, a metadata-minimal persistent key tombstone returns `409 idempotency_conflict` for the old key without creating a session or exposing deleted content. Session and operation IDs are never recycled. A new intentional upload uses a new key and operation identity. Normal restart with the original volume retains F2.2 resumability.

Single-file bytes are at `/artifacts/{id}/content`. A tree's `/inventory` lists original relative paths, files, size, SHA-256 and executable flags, plus explicit empty directories; each file has an opaque `fileId` for `/files/{file_id}`. Without filters, inventory lists all entries as before. `parent` lists direct children of an original directory (`parent=` means the root); a nonexistent or non-directory parent returns 404. `q` searches exact UTF-8 path substrings (case-sensitive, at most 256 UTF-8 bytes). `parent` and nonempty `q` cannot be combined. Cursors are bound to the filter, principal, Artifact and page size; changing any of these invalidates the cursor. Clients can construct a tree directly without downloading an archive. Download supports RFC 9110 `Range`, strong stable ETag and `If-Range`: a matching validator permits `206` and `Content-Range`; nonmatching `If-Range` returns the full `200`; an unsatisfiable single range returns `416` with `Content-Range: bytes */total`. Single byte ranges, including suffix and open-ended forms, are supported; multiple ranges may be ignored with a full `200` response. No archive export is required. Authentication is checked before content and range responses.

`DELETE` of an App or Release starts permanent deletion and returns `202` plus an Operation ID only after access revocation is committed atomically with acceptance. Its response has `accessRevoked: true`; physical cleanup may remain pending. Association removal from a Draft Release also has an operation, because CAS cleanup may be needed. Operations expose `Accepted`, `Running`, `Completed` or `Failed`, and deletion separately exposes `accessRevoked` and `physicalCleanup` (`NotStarted`, `Pending`, `RetryableFailure`, `Completed`). A retryable cleanup failure remains visible and can be retried by the Server. Deletion reaches `Completed` only after all required managed-state and physical cleanup; no trash or restore window exists. Deleting the last Published Release returns its surviving App to Draft. Deletion does not erase offline copies or independent backups. The initiating principal and Server Admin can read the operation even after target access is revoked; other callers cannot infer it. It remains queryable while unfinished and for at least 24 hours after termination.

`GET /capabilities` reports the Server's actual API version, enabled ingestion modes, ZIP methods, identity formats, Range support, configured limits and optional features. Its public `build` object contains embedded official release version and Git revision, or null values for local and older builds without validated metadata. It includes `serverImport` in `features` when Inbox or another root is available; the authorized root list remains private to each user. Planned features are not advertised as enabled; sensitive storage layout is not exposed.

## Deployment and administrative operations

[Deployment and operations](deployment-operations.md) defines storage, backup, restore and recovery. `GET /health/live` returns minimal `200` when the process responds; `GET /health/ready` returns minimal `200 Ready` after mandatory recovery or `503 NotReady` while service is unsafe. These endpoints are unauthenticated and contain no Private data. Shutdown makes readiness false before the process exits.

Only Server Admin may start or read a backup, retrieve its members or read diagnostics. `POST /backups` requires `Idempotency-Key`, has no request body, and returns `202` with an `Operation` of kind `Backup` and `Location: /api/v1/operations/{id}`. Exact authorized retries return the same operation; conflicting reuse returns `409 idempotency_conflict`. The backup operation has the existing `Accepted`, `Running`, `Completed`, `Failed` states. Its `backupId` is present only when `Completed`; failure carries a safe `problem`. Completion means durable, verified portable output, not merely a SQLite snapshot. `GET /backups` lists only completed backups with cursor pagination. `GET /backups/{backup_id}` returns the versioned manifest; `GET /backups/{backup_id}/entries/{entry_id}` returns one immutable binary member with stable strong ETag and standard single-byte Range/If-Range behavior. Members are selected by opaque ID; manifest relative paths permit offline reconstruction but no physical host or CAS path is returned. An unsatisfiable member Range returns `416 range_not_satisfiable` as RFC 9457 Problem Details with `Content-Range: bytes */<complete-length>`. `404` hides unavailable or unauthorized backup IDs, `401` requires authentication, `403` rejects a known forbidden administrative action, `503 retryable_failure` reports temporarily unavailable storage. Backup responses use `Cache-Control: no-store`; the operator must protect downloaded credential-bearing data. Restore is offline CLI only.

For a `Backup` operation, `backupOutputAvailable` is always present. It is `false` before completion or after failure; a historical `Completed` operation retains its original `id`, `backupId` and status after restore, with `backupOutputAvailable: false` if the output is missing, incomplete or fails manifest/member verification. `GET /operations/{id}` returns that result to a current Server Admin. An exact authorized `POST /backups` replay with its original key returns `202`, the same operation ID and current availability flag, even if output is unavailable; the key cannot create or replace a backup. `GET /backups` lists only `Completed` output with verified availability. Direct manifest/member requests for unavailable output return the same `404 not_found` as unknown output. Availability is reassessed against independently present and verified local files; it never changes historical `Completed` to `Failed`. The historical operation and key binding remain for the lifetime of the restored catalog, so reuse cannot initiate another backup.

`GET /admin/diagnostics` is Server Admin-only and returns redacted aggregate status and metrics: readiness/recovery/migration, storage pressure, request failure and latency aggregates, active uploads/transfers, pending/retryable cleanup and backup counts. It never returns secrets, raw CAS paths, Private content or a configuration dump. Normal API authentication applies; cookie-authenticated state changes still require Origin/CSRF checks. Direct HTTP offers no confidentiality, so backup and administrative access require a protected transport in remote deployments.

## Errors

Each error has `type` exactly equal to `"urn:app-depot:problem:" + code`, matching stable `code`, `title`, numeric `status`, and optional safe `detail`, `instance` and `operationId`. It uses `application/problem+json`; readiness `503` is the sole health-probe exception and returns minimal `application/json` with `status: "NotReady"`. Hidden Private resources use the same `404 not_found` as absent resources. Relevant codes and statuses:

| Status | Codes |
| --- | --- |
| 400 | `invalid_request`, `invalid_upload_range`, `invalid_zip`, `invalid_tree`, `invalid_cursor` |
| 401 / 403 / 404 | `unauthenticated` / `forbidden` / `not_found` |
| 409 | `conflicting_upload_bytes`, `incomplete_upload`, `idempotency_conflict`, `immutable_release`, `invalid_channel_target`, `operation_running`, `unsupported_artifact_kind`, `progress_changed` |
| 410 / 412 / 428 | `session_expired`, `session_cancelled` / `stale_etag` / `precondition_required` |
| 413 / 422 | `quota_exceeded`, `image_too_large` / `hash_mismatch`, `unsupported_zip_method`, `invalid_image`, `unsupported_image` |
| 416 / 429 / 507 / 503 | `range_not_satisfiable` / `rate_limited` / `insufficient_storage` / `retryable_failure` |

Invalid upload `Content-Range` returns `400`, never download-style `416`. A range whose declared total conflicts with the session is `400`; a byte conflict with accepted staging is `409`. Error bodies do not reveal hidden resource details.

## HTTP scenarios

The examples use fixed illustrative IDs and omit unrelated response headers. `Authorization: Bearer OWNER` represents an authorized token; `ADMIN` represents Server Admin. JSON lines show complete bodies for the represented exchange. Scenarios are independent unless explicitly linked; binary-body labels state byte counts rather than printing raw bytes. Channel targets are Published Releases of the shown App; anonymous-read and Private examples assume the stated visibility and Server setting.

### 1. Minimal App and explicit publication

```http
POST /api/v1/uploads HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: minimal-1
Content-Type: application/json

{"mode":"BareFile","target":{"newAppName":"Demo"},"file":{"size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"}}

HTTP/1.1 201 Created
Location: /api/v1/uploads/44444444-4444-4444-8444-444444444444
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"BareFile","phase":"Receiving","inventoryComplete":true,"target":{"newAppName":"Demo"},"file":{"path":"a.txt","type":"File","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 0-0/1

A

HTTP/1.1 204 No Content

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/finalize HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: minimal-finalize-1

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}

POST /api/v1/releases/22222222-2222-4222-8222-222222222222/publish HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: minimal-publish-1
If-Match: "release-1"

HTTP/1.1 200 OK
ETag: "release-2"
Content-Type: application/json

{"id":"22222222-2222-4222-8222-222222222222","appId":"11111111-1111-4111-8111-111111111111","state":"Published","version":null}
```

### 2. Bare single-file upload to existing Draft Release

```http
POST /api/v1/uploads HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: file-2
Content-Type: application/json

{"mode":"BareFile","target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"}}

HTTP/1.1 201 Created
Location: /api/v1/uploads/44444444-4444-4444-8444-444444444444
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"BareFile","phase":"Receiving","inventoryComplete":true,"target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"path":"a.txt","type":"File","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 0-0/1

A

HTTP/1.1 204 No Content

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/finalize HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: file-finalize-2

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}
```

### 3. Inventory-first directory upload

```http
POST /api/v1/uploads HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: tree-3
Content-Type: application/json

{"mode":"BareTree","target":{"releaseId":"22222222-2222-4222-8222-222222222222"}}

HTTP/1.1 201 Created
Location: /api/v1/uploads/44444444-4444-4444-8444-444444444444
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"BareTree","phase":"Receiving","inventoryComplete":false,"target":{"releaseId":"22222222-2222-4222-8222-222222222222"}}

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/inventory HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: tree-inventory-3
Content-Type: application/json

{"entries":[{"path":"a.txt","type":"File","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}]}

HTTP/1.1 200 OK
Content-Type: application/json

{"entries":[{"path":"a.txt","type":"File","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}]}

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/inventory/complete HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: tree-inventory-complete-3

HTTP/1.1 200 OK
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"BareTree","phase":"Receiving","inventoryComplete":true,"target":{"releaseId":"22222222-2222-4222-8222-222222222222"}}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 0-0/1

A

HTTP/1.1 204 No Content

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/finalize HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: tree-finalize-3

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}
```

### 4. Explicit ZIP import

```http
POST /api/v1/uploads HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: zip-4
Content-Type: application/json

{"mode":"ZipImport","target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"size":109,"sha256":"d885a2eb071385750f1bf51962db40bf2cb8906d4a5ced030b31d92d4e39703b"}}

HTTP/1.1 201 Created
Location: /api/v1/uploads/44444444-4444-4444-8444-444444444444
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"ZipImport","phase":"Receiving","inventoryComplete":true,"target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"path":"bundle.zip","type":"File","fileId":"66666666-6666-4666-8666-666666666666","size":109,"sha256":"d885a2eb071385750f1bf51962db40bf2cb8906d4a5ced030b31d92d4e39703b","executable":false}}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 0-108/109
Content-Length: 109

<109 binary ZIP bytes: Store member a.txt containing A>

HTTP/1.1 204 No Content

POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/finalize HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: zip-finalize-4

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}
```

### 5. Resume a large file

```http
POST /api/v1/uploads HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: large-5
Content-Type: application/json

{"mode":"BareFile","target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"size":4096,"sha256":"6896d9ea3f73a4434f5832bc65714e7d066f177373f36f34dc8a6f735daa41b1"}}

HTTP/1.1 201 Created
Location: /api/v1/uploads/44444444-4444-4444-8444-444444444444
Content-Type: application/json

{"id":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","mode":"BareFile","phase":"Receiving","inventoryComplete":true,"target":{"releaseId":"22222222-2222-4222-8222-222222222222"},"file":{"path":"big.bin","type":"File","fileId":"66666666-6666-4666-8666-666666666666","size":4096,"sha256":"6896d9ea3f73a4434f5832bc65714e7d066f177373f36f34dc8a6f735daa41b1","executable":false}}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 0-1023/4096
Content-Length: 1024

<1024 bytes of A>

HTTP/1.1 204 No Content

GET /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666/progress?pageSize=100 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","fileId":"66666666-6666-4666-8666-666666666666","totalSize":4096,"progressVersion":1,"ranges":[{"start":0,"end":1024}],"nextCursor":null}

PUT /api/v1/uploads/44444444-4444-4444-8444-444444444444/files/66666666-6666-4666-8666-666666666666 HTTP/1.1
Authorization: Bearer OWNER
Content-Type: application/octet-stream
Content-Range: bytes 1024-4095/4096
Content-Length: 3072

<3072 bytes of A>

HTTP/1.1 204 No Content
```

### 6. Lost finalization response

```http
POST /api/v1/uploads/44444444-4444-4444-8444-444444444444/finalize HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: tree-finalize-3

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}

GET /api/v1/uploads/44444444-4444-4444-8444-444444444444/result HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
Content-Type: application/json

{"sessionId":"44444444-4444-4444-8444-444444444444","operationId":"55555555-5555-4555-8555-555555555555","phase":"Completed","artifactId":"adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1","appId":"11111111-1111-4111-8111-111111111111","releaseId":"22222222-2222-4222-8222-222222222222","associationId":"33333333-3333-4333-8333-333333333333"}
```

### 7. Read immutable Artifact inventory

```http
GET /api/v1/artifacts/adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1/inventory?pageSize=100 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "adtree-v1-49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1"
Content-Type: application/json

{"items":[{"type":"File","path":"a.txt","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}],"nextCursor":null}
```

### 8. Download with Range and If-Range

```http
GET /api/v1/artifacts/adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd/content HTTP/1.1
Authorization: Bearer OWNER
Range: bytes=0-0
If-Range: "adfile-559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"

HTTP/1.1 206 Partial Content
ETag: "adfile-559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"
Accept-Ranges: bytes
Content-Range: bytes 0-0/1
Content-Length: 1
Content-Type: application/octet-stream

A

GET /api/v1/artifacts/adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd/content HTTP/1.1
Authorization: Bearer OWNER
Range: bytes=0-0
If-Range: "stale"

HTTP/1.1 200 OK
ETag: "adfile-559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"
Accept-Ranges: bytes
Content-Length: 1
Content-Type: application/octet-stream

A
```

### 9. Read and update declarations with ETag

```http
GET /api/v1/apps/11111111-1111-4111-8111-111111111111/declarations HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "decl-app-1"
Content-Type: application/json

{"revisionId":"77777777-7777-4777-8777-777777777777","data":{}}

PUT /api/v1/apps/11111111-1111-4111-8111-111111111111/declarations HTTP/1.1
Authorization: Bearer OWNER
If-Match: "decl-app-1"
Content-Type: application/json

{"data":{"description":"Demo"}}

HTTP/1.1 200 OK
ETag: "decl-app-2"
Content-Type: application/json

{"revisionId":"99999999-9999-4999-8999-999999999999","data":{"description":"Demo"}}
```

### 10. Consistent declaration snapshot

```http
GET /api/v1/releases/22222222-2222-4222-8222-222222222222/declaration-snapshot HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "snapshot-1"
Content-Type: application/json

{"releaseId":"22222222-2222-4222-8222-222222222222","app":{"revisionId":"77777777-7777-4777-8777-777777777777","data":{"description":"Demo"}},"release":{"revisionId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","data":{}},"associations":[{"associationId":"33333333-3333-4333-8333-333333333333","declaration":{"revisionId":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","data":{}}}]}
```

### 11. Create and move a channel

```http
PUT /api/v1/apps/11111111-1111-4111-8111-111111111111/channels/stable HTTP/1.1
Authorization: Bearer OWNER
If-None-Match: *
Content-Type: application/json

{"releaseId":"22222222-2222-4222-8222-222222222222"}

HTTP/1.1 201 Created
ETag: "channel-1"
Location: /api/v1/apps/11111111-1111-4111-8111-111111111111/channels/stable
Content-Type: application/json

{"appId":"11111111-1111-4111-8111-111111111111","name":"stable","releaseId":"22222222-2222-4222-8222-222222222222"}

PUT /api/v1/apps/11111111-1111-4111-8111-111111111111/channels/stable HTTP/1.1
Authorization: Bearer OWNER
If-Match: "channel-1"
Content-Type: application/json

{"releaseId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}

HTTP/1.1 200 OK
ETag: "channel-2"
Content-Type: application/json

{"appId":"11111111-1111-4111-8111-111111111111","name":"stable","releaseId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}
```

### 12. Revoke Publish while retaining ownership

```http
PATCH /api/v1/users/88888888-8888-4888-8888-888888888888 HTTP/1.1
Authorization: Bearer ADMIN
If-Match: "user-1"
Content-Type: application/json

{"publishPermission":false}

HTTP/1.1 200 OK
ETag: "user-2"
Content-Type: application/json

{"id":"88888888-8888-4888-8888-888888888888","username":"owner","serverAdmin":false,"publishPermission":false}

PATCH /api/v1/apps/11111111-1111-4111-8111-111111111111 HTTP/1.1
Authorization: Bearer OWNER
If-Match: "app-1"
Content-Type: application/json

{"name":"Demo 2"}

HTTP/1.1 200 OK
ETag: "app-2"
Content-Type: application/json

{"id":"11111111-1111-4111-8111-111111111111","name":"Demo 2","slug":"demo","ownerId":"88888888-8888-4888-8888-888888888888","visibility":"Private","state":"Draft"}

POST /api/v1/releases/22222222-2222-4222-8222-222222222222/publish HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: publish-after-revoke
If-Match: "release-1"

HTTP/1.1 403 Forbidden
Content-Type: application/problem+json

{"type":"urn:app-depot:problem:forbidden","title":"Forbidden","status":403,"code":"forbidden"}
```

### 13. Anonymous Public content when enabled

```http
GET /api/v1/artifacts/adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd/content HTTP/1.1

HTTP/1.1 200 OK
ETag: "adfile-559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"
Accept-Ranges: bytes
Content-Length: 1
Content-Type: application/octet-stream

A
```

### 14. Hide unauthorized Private content

```http
GET /api/v1/artifacts/adfile:sha256:559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd/content HTTP/1.1
Authorization: Bearer OTHER

HTTP/1.1 404 Not Found
Content-Type: application/problem+json

{"type":"urn:app-depot:problem:not_found","title":"Not Found","status":404,"code":"not_found"}
```

### 15. Permanent deletion and status polling

```http
DELETE /api/v1/apps/11111111-1111-4111-8111-111111111111 HTTP/1.1
Authorization: Bearer OWNER
If-Match: "app-2"
Idempotency-Key: delete-app-15

HTTP/1.1 202 Accepted
Location: /api/v1/operations/55555555-5555-4555-8555-555555555555
Content-Type: application/json

{"id":"55555555-5555-4555-8555-555555555555","kind":"DeleteApp","status":"Accepted","accessRevoked":true,"physicalCleanup":"Pending"}

GET /api/v1/operations/55555555-5555-4555-8555-555555555555 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
Content-Type: application/json

{"id":"55555555-5555-4555-8555-555555555555","kind":"DeleteApp","status":"Running","accessRevoked":true,"physicalCleanup":"Pending"}

GET /api/v1/operations/55555555-5555-4555-8555-555555555555 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
Content-Type: application/json

{"id":"55555555-5555-4555-8555-555555555555","kind":"DeleteApp","status":"Completed","accessRevoked":true,"physicalCleanup":"Completed"}
```

### 16. Expired upload session

```http
GET /api/v1/uploads/44444444-4444-4444-8444-444444444444 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 410 Gone
Content-Type: application/problem+json

{"type":"urn:app-depot:problem:session_expired","title":"Upload session expired","status":410,"code":"session_expired"}
```

### 17. Conflicting Idempotency-Key

```http
POST /api/v1/apps HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: app-create-17
Content-Type: application/json

{"name":"First"}

HTTP/1.1 201 Created
Location: /api/v1/apps/11111111-1111-4111-8111-111111111111
ETag: "app-1"
Content-Type: application/json

{"id":"11111111-1111-4111-8111-111111111111","name":"First","slug":"first","ownerId":"88888888-8888-4888-8888-888888888888","visibility":"Private","state":"Draft"}

POST /api/v1/apps HTTP/1.1
Authorization: Bearer OWNER
Idempotency-Key: app-create-17
Content-Type: application/json

{"name":"Second"}

HTTP/1.1 409 Conflict
Content-Type: application/problem+json

{"type":"urn:app-depot:problem:idempotency_conflict","title":"Idempotency key conflict","status":409,"code":"idempotency_conflict"}
```

### 18. Large paginated inventory

```http
GET /api/v1/artifacts/adtree:v1:sha256:de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f/inventory?pageSize=1 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "adtree-v1-de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f"
Content-Type: application/json

{"items":[{"type":"File","path":"a.txt","fileId":"66666666-6666-4666-8666-666666666666","size":1,"sha256":"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd","executable":false}],"nextCursor":"opaque-1"}

GET /api/v1/artifacts/adtree:v1:sha256:de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f/inventory?pageSize=1&cursor=opaque-1 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "adtree-v1-de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f"
Content-Type: application/json

{"items":[{"type":"Directory","path":"empty"}],"nextCursor":"opaque-2"}

GET /api/v1/artifacts/adtree:v1:sha256:de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f/inventory?pageSize=1&cursor=opaque-2 HTTP/1.1
Authorization: Bearer OWNER

HTTP/1.1 200 OK
ETag: "adtree-v1-de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f"
Content-Type: application/json

{"items":[{"type":"Directory","path":"empty/nested"}],"nextCursor":null}
```
