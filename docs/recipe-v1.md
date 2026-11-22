# Derived Applications and RecipeV1

A Derived App has its own UUID, owner, visibility, artwork, declarations, Releases and channels. A Derived Release owns a versioned Recipe instead of direct Artifact associations. The Recipe is the release composition. Standard Releases continue to own Artifact associations and use the existing upload and publication rules. Neither kind changes File, Tree or CAS identity. A Recipe is distinct from the editable declaration revisions of its Release.

## Definition

`version` is the integer `1`. A definition has a `dependencies` object and a `targets` object. Property names are unique. Unknown members and unsupported selector types or versions are rejected. Optional nullable selection fields treat null as omission. [OpenAPI](openapi.yaml) describes the wire format.

```json
{
  "version": 1,
  "dependencies": {
    "resources": {
      "appId": "11111111-1111-4111-8111-111111111111",
      "selector": { "type": "release", "releaseId": "22222222-2222-4222-8222-222222222222" }
    },
    "engine": {
      "appId": "33333333-3333-4333-8333-333333333333",
      "selector": { "type": "channel", "name": "stable" }
    }
  },
  "targets": {
    "linux/x86_64": {
      "operations": [
        { "dependency": "resources", "source": "game/data", "destination": "data" },
        { "dependency": "engine", "destination": "bin/engine", "executable": true }
      ],
      "launch": {
        "path": "bin/engine",
        "workingDirectory": "",
        "arguments": ["--assets", "data"]
      }
    }
  }
}
```

This example expects one Tree Artifact in the resources release and one File Artifact in the current engine channel release. To follow each new publication for the engine, use `"selector":{"type":"latest"}`. If each engine release has separate Artifacts labelled `Linux engine` and `Windows engine`, set `"associationLabel":"Linux engine"` on the Linux copy and choose the other label in a separate Windows target. A label must match exactly once in the selected release. Exact association IDs can be used with pinned releases or when an author deliberately intends to bind to the current association. Changing a release normally changes its association IDs.

### Selectors and platform constraints

| Selector | Meaning |
| --- | --- |
| `{"type":"release","releaseId":"<uuid>"}` | Exactly that Published Release of the declared App. |
| `{"type":"channel","name":"stable"}` | The current Published Release referenced by that case-sensitive channel on the App. |
| `{"type":"latest"}` | The most recently published surviving Release on the App, by Server publication order. Drafts, version text, edits and channel moves have no effect on this order. |

App, Release and association references use canonical lowercase UUIDs with hyphens. The alias is local to the Recipe and contains 1–64 ASCII letters, digits, underscores or hyphens. Declare 1–32 aliases and reference each in at least one target operation. The channel name is 1–255 UTF-8 bytes and excludes slash and control characters.

Targets are keyed by the exact host OS and architecture, using `linux/x86_64`, `linux/aarch64`, `windows/x86_64`, `windows/aarch64`, `macos/x86_64` or `macos/aarch64`. Resolution considers only operations for the requested target. A dependency may declare a nonempty, distinct `platforms` array of these same tokens to explicitly constrain its use. A Derived dependency supplies its assembled tree for the same platform. Standard App and Release declarations remain descriptive for recipes. For example, an Artifact stored with Windows metadata may contain data files used by a Linux target. App Depot does not infer functional compatibility, binary architecture or eligibility from filenames, declarations, visible versions or version ranges.

### Copy operations and paths

A target has 1–128 operations, processed in array order. Each operation copies from a dependency alias. For a Standard dependency, select its sole association by leaving both selector fields absent, its exact `associationId`, or its case-sensitive `associationLabel` from the association's current `data.appDepot.label` declaration. A label is a nonblank string of at most 160 characters with no controls. Supply at most one selector. Multiple matches or zero matches are errors. For a Derived dependency, both association selector fields are absent and the source is the child's resolved virtual tree.

| Source | `source` | `destination` |
| --- | --- | --- |
| Whole File Artifact | Omitted or empty | Exact output file path, required and nonempty |
| Whole Tree Artifact or Derived output | Omitted or empty | Output directory, empty for installation root |
| Directory within a tree | Exact original directory path | Output directory, empty for installation root. The selected directory's contents go directly there. |
| File within a tree | Exact original file path | Exact output file path, required and nonempty |

Multiple operations copy a subset of files or directories. The Server preserves empty directories and generates missing parent directories. Directory merges are allowed. File-to-file writes to the same path fail by default, even for identical content. An operation with `"collision":"replaceFile"` may replace an earlier file at the exact same path. File–directory conflicts always fail. The Server uses exact spellings and additionally rejects lowercase-equivalent paths, DOS device names, disallowed characters and trailing dots or spaces on Windows targets. The client must independently reject aliases, normalization conflicts and unrepresentable names on the actual destination filesystem.

Nonempty paths follow [Tree Identity v1](tree-identity.md) syntax and are limited to 1,024 UTF-8 bytes and 32 components. They reject absolute paths, empty components, `.`, `..`, backslashes, colons and control characters. An empty path is accepted only where explicitly stated. The rules apply again to assembled paths. Symlinks, devices and host paths are outside the model.

Tree files inherit their portable executable flag. File Artifacts begin with the flag cleared. An individual file copy may set `"executable":true`. A directory copy preserves each file's flag. A file replacement uses the winning source's flag and any override on that operation.

### Launch entry point

The Recipe itself selects one output file per platform as `launch.path`. This explicitly defines launch for the assembled tree independently of association entry-point declarations. The path must identify a final regular file with its executable flag set for Linux and macOS. On Windows the flag carries no launch eligibility requirement. `workingDirectory` is an existing output directory, with the empty string or omission meaning installation root. `arguments` defaults to an empty list and contains at most 64 literal UTF-8 strings, at most 4,096 bytes each and 16,384 bytes in total. Control characters are forbidden. Empty arguments are representable in the API. Each string is a single process argument.

RecipeV1 provides only confined file copies and direct native launch metadata. There are no script hooks, interpreters, shell expansion, external downloads, environment overrides, installers, runtime preparation mechanisms or dependency installation side effects.

## Resolution and publication

Draft replacement checks structure and the Release ETag. It can save an unresolved definition for further editing. Publishing requires the same strong Release ETag and an idempotency key. The Server revalidates the stored version and structure, resolves every target for the publishing principal, and publishes atomically with that check. Every declared target must be resolvable at publication. Publication does not save or reserve a lock. Later changes to selection, declarations used for association labels, visibility or deletion may change or prevent resolution.

Resolution takes a single SQLite snapshot of the relevant catalog authorization, channel pointers, publication ordering, recipes, descriptions used as selectors, associations and immutable inventories. It uses the identified request principal and existing catalog read rules independently for every dependency. The request does not borrow the Derived App owner's privileges. Only Published Releases qualify as dependencies, including for owners and admins. API authentication occurs before the catalog snapshot. The published definition itself is accessible to managers. Other readers must be able to resolve all its targets before receiving the raw definition, which can include references for multiple targets. A caller can request a lock for one authorized target directly, regardless of whether another target resolves. Responses containing recipe or lock data are marked `Cache-Control: no-store`.

The resolver walks each target's dependency graph in operation order. Revisiting an App UUID in an active branch is a cycle, even with another Release of that App. Using different Releases of the same App anywhere in one graph is a conflict. Multiple aliases may resolve to the same Release. Missing Apps or Releases, unauthorized dependencies, unknown channels, an exact Release belonging to a different App and deleted or unpublished targets are all reported as `dependency_unavailable`, without disclosing which hidden object exists. Deleting a dependency does not delete or rewrite the referring Recipe. A future resolve has no fallback for a missing pinned Release or channel. Latest can select the next most recently published surviving Release after deletion.

Resolution is bounded to 16 simultaneously active Derived Releases, 128 distinct Apps including the root, 50,000 inventory or output entries per tree and 200,000 units of total work across operations and inventory/Tree traversal. The Server rejects oversized graphs with `recipe_limit`.

## Lock and installation state

The returned version 1 lock has the root `appId`, `releaseId` and `platform`, and includes the following fields.

| Field | Contents |
| --- | --- |
| `releases` | Map keyed by Release UUID, including the root. Each entry gives the owning App UUID, its optional display version, dependency aliases mapped to resolved Release UUIDs, and used association UUIDs mapped to exact typed Artifact IDs. |
| `files` | Map keyed by final destination file path. Each winning source gives its association ID, typed Artifact ID, Tree `fileId` or null for a standalone File, SHA-256, exact byte size and final executable flag. |
| `directories` | Sorted final relative directories, including explicit empty ones and all required parents. The installation root is implicit. |
| `launch` | Final file, working directory and literal arguments for the requested platform. |

Equal catalog inputs and principal yield the same ordered maps, layout and lock payload. The lock has no clock time, random ID, signed URL or mutable channel reference. Display version labels are informational and may change independently of identity. Persist the resolved lock with an install transaction to reproduce it after channel movement, new publication or declaration edits. Do not resolve moving selectors again while downloading or repairing a pinned install. The lock grants no privileges, retains no CAS references, reserves no available bytes and is not an installed-state record. Later downloads apply current access rules. A deleted association or Artifact or revoked access may prevent an online replay. Download endpoints verify CAS integrity and can report unavailable or corrupt storage independently of metadata resolution. A recipe resolution does not read or hash all payload bytes.

### Client requirements

M12.2 must implement parsing and safe application of the lock, Artifact and per-file verification against File and Tree Identity v1, per-device authorization and confined staging. A client must use the normal authorized Artifact download endpoints, validate the complete layout against the destination filesystem, reject symlinks and unrepresentable paths, and verify lengths and digests before materialization. Download and assemble using a fresh staging directory inside a client-controlled installation area. Keep independent application roots and never write to a dependency's local installation or verified content cache. Publish the new installation and its lock atomically using the client's recovery model. A failed, interrupted or cancelled installation or update leaves the previous installation and its recorded lock intact. Updates start through explicit client intent and a new resolution. Verify the chosen launch file and working directory before directly creating the declared process, with the user's authority. Existing Standard installations and operation records keep their current semantics.

M12.3 will add GitHub Releases discovery, explicit App mapping and a verified import/publication workflow with upstream provenance, idempotency and error handling. Automated scheduling is outside M12.1. An upstream publication can affect a future latest/channel resolution according to the usual selector rules.

The later real-world acceptance test uses the owner's separately uploaded Windows game data and an independently maintained dhewm3 release. M12.1 exercises equivalent Tree resources, a native executable fixture and an independent Derived App entirely with synthetic content.
