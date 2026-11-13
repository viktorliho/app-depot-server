# Agent governance

## Authority and scope

- Accepted [architecture](docs/architecture.md), [Tree Identity v1](docs/tree-identity.md), [reliable upload and commit](docs/upload-commit.md), [API v1](docs/api.md), [OpenAPI 3.1](docs/openapi.yaml), and [deployment and operations](docs/deployment-operations.md) govern product behavior and external contracts. This file governs agent conduct, repository hygiene and operational permission. The current owner instruction defines the immediate task within those boundaries.
- MUST read applicable contracts before changing behavior. A newer prompt does not silently supersede an accepted contract. An explicit owner task may authorize replacing a specific pre-1.0 contract under the policy below; otherwise, if it conflicts, request owner clarification. Internal engineering decisions are allowed where no approved requirement is contradicted. A material contradiction between authoritative documents is a Hard Stop.
- This file grants no standing authorization for new features, architectural changes, deployment or publication. Follow the current owner instruction and the authority boundaries below.

## Pre-1.0 compatibility policy

Until the owner explicitly declares App Depot Server version 1.0:

- Breaking changes to APIs, contracts, configuration, storage schemas and internal interfaces are permitted within an explicitly authorized task. Backward compatibility with previous pre-1.0 versions is not guaranteed.
- MUST NOT add legacy endpoints, compatibility wrappers, deprecated parallel implementations or migration complexity solely to preserve earlier pre-1.0 behavior unless the task explicitly requires them.
- An authorized change may replace an existing contract directly. Update implementation, OpenAPI, normative documentation and tests together so the repository describes current behavior. Breaking changes still require an authorized task and MUST NOT appear silently during unrelated work.
- MUST NOT silently destroy existing user data. For incompatible storage changes, clearly document whether migration, backup/restore or fresh initialization is required; require an explicit operator action before abandoning old data. Security, integrity, authorization and durability guarantees remain mandatory.
- Only the owner may declare version 1.0 and establish a stable compatibility policy.

## Authorized autonomy

- Within an explicitly authorized task, MAY make ordinary local engineering decisions, fix defects and bounded technical debt, run tests and isolated Dev/Preview instances, maintain deterministic fixtures, update existing documentation as required, and commit verified changes locally. Authorization for one task does not authorize unrelated development.
- MUST stop for owner direction before changing accepted contracts unless the current task explicitly authorizes that change; weakening identity, authorization, security, durability, recovery or deletion; adding significant runtime dependencies; changing the approved stack or deployment architecture; introducing product requirements; publishing, pushing or creating remotes; deploying to real infrastructure; or destructive work outside isolated development data. Do not modify external repositories.

## Repository and documentation hygiene

- Keep only current product source, required assets/manifests/migrations, necessary tests and deterministic fixtures, authorized CI/deployment files, current normative documents, and essential developer/operator instructions. Before adding a permanent file, establish its durable purpose and whether an existing file can serve it.
- MUST NOT commit agent progress reports, temporary plans, research notes, debugging logs, test output, temporary screenshots/recordings, disposable scripts, generated caches/build outputs, temporary archives, credentials, secrets, or duplicate/obsolete documentation. Keep disposable diagnostics and review artifacts in a dedicated temporary workspace outside the repository. Generated source, reproducible fixtures or release artifacts belong in Git only when the build, tests or distribution contract genuinely requires them.
- Documentation describes the current accepted system in English. Update the existing authoritative document; remove obsolete statements rather than append history. Create a new document only for a distinct durable contract that cannot reasonably fit an existing one. Git history records past changes. MUST NOT silently edit an accepted external contract; if implementation exposes a defect that requires an unauthorized change, Hard Stop and request approval.

## Architecture and local work

- Preserve the Rust modular monolith and its existing Server, application, domain and infrastructure boundaries. Prefer the smallest complete solution. Every abstraction needs a demonstrated responsibility; avoid speculative frameworks, extension points, wrappers, factories, managers, traits and switches. Do not duplicate behavior or retain obsolete internal compatibility paths without a current requirement. Refactoring removes superseded code. Do not add unrelated features or start broad refactoring to make apparent progress.
- Fix bounded local debt during relevant work when scope, contracts and risk permit. Report an unresolved correctness defect; use Hard Stop if it blocks the required behavior. Significant runtime dependencies or architecture changes require owner approval.
- Use isolated Dev/Preview data roots and the real Server, SQLite, CAS and API. Never infer a destructive reset target from the current directory; never reset Production data through development helpers. Keep credentials, logs and caches out of tracked source. No dev-only authentication shortcut in Production. Follow the deployment and operations contract for storage, bootstrap, security and recovery. Do not change host-wide configuration or unrelated user files during routine development.

## Hard Stop

Hard Stop is mandatory if: a prompt conflicts with an accepted contract without explicitly authorizing its replacement; authoritative documents materially conflict; required behavior has materially different interpretations without a decision; implementation needs weaker identity, authorization, security, durability, recovery or deletion; a necessary filesystem/SQLite guarantee cannot be established; progress requires bypassing a required test; required verification remains impossible after reasonable investigation; previously reported verification is discovered to have been invalid; repository state cannot be reconstructed reliably; work exceeds authority; real deployment, publishing, remotes or destructive work outside isolated development data is needed; or a significant new requirement or architectural change becomes necessary. Investigate using repository evidence, contracts and tests first; ordinary local choices do not need approval.

An ordinary failing test during authorized work does not itself require Hard Stop. Investigate and fix a regression in previously verified behavior when the fix stays within accepted contracts and authorized scope; the work remains incomplete until its tests and affected prior checks pass again. If the regression requires an unauthorized contract, architecture, security or durability change, Hard Stop applies. Never weaken, skip or misreport required tests to obtain an apparent pass.

On Hard Stop, MUST:

1. Stop new blocked implementation and prevent unsafe operations; preserve the working tree. Do not automatically revert useful work or hide it in a WIP commit.
2. Keep diagnostics outside the repository. Report the exact issue with file/line evidence, consequences, smallest viable resolution options, Git status, checks completed and work unfinished.
3. Wait for explicit owner direction before continuing blocked work. Do not invent a fallback, silently choose an interpretation or mark the work complete.

## Verification and commits

- An authorized change is complete only when applicable tests pass, behavior works against the real Server where relevant, affected prior checks still pass, relevant Preview is current, and the work is committed locally. Run deterministic recovery, API/authorization and browser checks where applicable. Capabilities and readiness MUST NOT claim unfinished behavior. Fast deterministic checks belong in the normal loop; extended fault campaigns and physical power-loss tests remain release-level requirements.
- MUST NOT disable tests to get green, remove failing assertions without contract evidence, replace required integration with mocks, ignore reproducible failures, report skipped/unavailable checks as passed, or claim tests not run. Correct an erroneous test against the authoritative contract and explain the evidence.
- Each local commit SHOULD have one coherent, verified purpose. Before committing, review the diff, staged paths, secrets and disposable artifacts; run formatting and relevant tests; check for accidental contract changes and incomplete required behavior. No routine WIP commits, history rewriting, remotes, pushes or publication without owner authorization. Do not create progress report files merely to record completion.

## Reorientation

- At the start of each substantial task and after substantial context loss, re-read this file and applicable normative documents; inspect Git status/recent commits, existing implementation and relevant tests. Establish remaining work from repository evidence, not an agent summary alone. Investigate uncertain implementation or test state; Hard Stop if reliable reconstruction is impossible. This checkpoint stays internal and creates no repository document.
