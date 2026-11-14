# App Depot Server

App Depot Server is a self-hosted service for storing, cataloging, publishing, and distributing software artifacts. An App can publish a file or a directory tree with at least one regular file. Software-specific metadata is optional. The Server never executes uploaded content; clients handle installation and execution.

The Rust Server serves an HTTP API and Web UI backed by SQLite and a content-addressed store. It supports resumable uploads, private and public catalog entries, optional App artwork, versioned declarations, durable deletion, and portable backup and offline restore. The API contract is [OpenAPI 3.1](docs/openapi.yaml), with semantics in [API v1](docs/api.md).

## Compatibility (pre-1.0)

App Depot Server is pre-1.0. APIs, storage schemas and configuration may change incompatibly between revisions; backward compatibility is not guaranteed. Such changes require an explicit owner-authorized task and coordinated updates to the implementation, OpenAPI, technical contracts and tests. Existing user data must not be silently discarded: an incompatible storage change must state whether migration, backup/restore or fresh initialization is required. Security, integrity, authorization and durability guarantees remain mandatory. Only the owner can declare version 1.0 and a stable compatibility policy.

## Local development

Use Rust 1.88.0, Node 26.5.0, and npm 11.17.0 as pinned by `rust-toolchain.toml`, `web/.node-version`, and `web/package.json`. Keep development data outside the repository and separate from Production data.

```sh
mkdir -p /tmp/app-depot-local
cargo run -p app-depot-server -- init-dev --namespace /tmp/app-depot-local --instance dev
npm --prefix web ci
```

Start the Server and Web development server in separate terminals:

```sh
cargo run -p app-depot-server -- serve --namespace /tmp/app-depot-local --instance dev --mode dev --bind 127.0.0.1:8787
```

```sh
npm --prefix web run dev
```

Open `http://127.0.0.1:5173/`. Copy the `initial_setup` token from the Server terminal into the first-run form, then choose an Admin password. The data root retains the session key across restarts. Dev and Preview run natively on macOS and Linux without a container runtime.

## Deployment and verification

The [deployment and operations contract](docs/deployment-operations.md) defines storage, first-run setup, non-root execution, reverse-proxy security, backup, restore and recovery. [TrueNAS installation and updates](docs/deployment-operations.md#truenas-scale-2510-installation-and-updates) use the published `linux/amd64` image and [Compose example](compose.yaml). No local build or external bootstrap secret is needed.

Run `cargo test --workspace --locked` and `npm --prefix web run build` for the main local checks. Release acceptance remains pending for external deployment and durability gates; this project does not claim Production readiness.

## Technical contracts

- [Architecture](docs/architecture.md)
- [Tree Identity v1](docs/tree-identity.md)
- [Reliable upload and commit](docs/upload-commit.md)
- [API v1](docs/api.md) and [OpenAPI 3.1](docs/openapi.yaml)
- [Deployment and operations](docs/deployment-operations.md)
- [Portal UX architecture](docs/portal-ux.md)

## License

MIT. See [LICENSE](LICENSE).
