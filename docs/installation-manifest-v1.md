# InstallationManifestV1

InstallationManifestV1 is an optional, Release-scoped description of how to prepare an application's installed files and prerequisites. Its location is `data.appDepot.installation` in the normal Release declaration. Artifact identity and associations describe verified content. Association declarations can describe acquisition and launch. The installation manifest selects content and describes preparation. A Derived Release may delegate composition and launch to its existing RecipeV1. Ingestion format is irrelevant to the manifest.

## Representation and validation

`version` MUST be integer `1`. Unknown members, unsupported operations and unsupported versions are rejected. Optional lists default to empty and optional booleans to false. The normal Release declaration PUT validates the recognized property, including all association and source references, in the same transaction as the strong ETag check and revision write. Absence is valid for any Release. Remove the property to remove the configuration. Normal publication rules do not require a manifest or resolve all option combinations. Published Release declarations remain editable by their existing managers. Committed revisions are immutable.

Example for a Tree containing common files, translations and an explicitly verified installer:

```json
{
  "version": 1,
  "payload": [
    {"associationId":"11111111-1111-4111-8111-111111111111","source":"game","destination":""}
  ],
  "options":[
    {"id":"language","name":"Game language","language":true,"required":true,"defaults":["en-US"],"choices":[
      {"id":"en-US","name":"English","payload":[]},
      {"id":"fr-FR","name":"French","payload":[
        {"associationId":"11111111-1111-4111-8111-111111111111","source":"translations/fr","destination":"data","collision":"replaceFile"}
      ]}
    ]}
  ],
  "prerequisites":[
    {"id":"runtime","name":"Game runtime","associationId":"11111111-1111-4111-8111-111111111111",
      "path":"extras/runtime.exe","platforms":["windows/x86_64"],"arguments":["/quiet"],
      "evidence":"Publisher verified the path, arguments and execution order"}
  ],
  "unresolved":["Original registry integration has not been verified."]
}
```

## Payload and options

A Standard manifest has at least one base `payload` copy. Every operation identifies a canonical UUID association of the same Release. An absent or empty `source` selects an entire File or Tree. A nonempty source selects an exact Tree file or directory. A file has an exact nonempty destination filename. A tree or directory copies its contents into the destination, with empty destination meaning the installation root. Empty directories and Tree executable flags are preserved. Standalone File Artifacts have no portable executable flag. All destinations are relative. The [RecipeV1 path and collision rules](recipe-v1.md#copy-operations-and-paths) apply, including 1,024 UTF-8 bytes, 32 components, Windows aliases and disallowed names. `collision` defaults to `reject`. Only `replaceFile` can overwrite an earlier file at the exact same path. File-directory conflicts always fail. A nonempty `platforms` list restricts a copy to those exact v1 platform tokens. A nonempty Release platform declaration also restricts resolution.

Apply base copies in array order, then each selected option in group order and choice declaration order. Any choice may add zero or more copies. `multiple:false` permits at most one choice. `required:true` requires at least one selection. `defaults` supplies selected choice IDs only for omitted groups. An explicit empty list suppresses defaults. Unknown groups, duplicate choices and invalid cardinality fail. The plan records the normalized selections. Group IDs, choice IDs and prerequisite IDs use 1–80 ASCII letters, digits, hyphens or underscores. Group and prerequisite IDs are distinct within their respective lists. Choice IDs are distinct ignoring ASCII case within a group. Names are nonblank and at most 160 UTF-8 bytes without controls.

At most one group has `language:true` and it is single-choice. Its IDs are language tags using a leading letter and hyphen-delimited subtags of 1–8 ASCII alphanumeric characters, for example `en-US`. Display names are publisher-defined. Put shared files in the base and each known language's actual mappings in its choice. Use explicit defaults only when supported by publisher evidence. An empty choice can mean the base already contains that language. A single confirmed language can use a single choice. Incomplete localization can remain in `unresolved`; installer UI languages alone do not establish selectable game languages.

Limits are 16 option groups, 32 choices per group, 128 total copy operations across base and choices, 32 executable prerequisites and 128 unresolved notes of 1–1,024 UTF-8 bytes. Resolution bounds each Tree and output to 50,000 entries and charges up to 200,000 source entries across selected operations.

## Prerequisites and launch

An executable prerequisite has a stable `id`, `name`, exact `associationId`, exact regular Tree `path` (empty only for File Artifacts), nonempty `platforms` and nonblank publisher `evidence` of up to 1,024 bytes. It may contain a `when` object with `optionId` and `choiceId`, literal `arguments`, `optional`, and `successExitCodes`. The exit-code list defaults to `[0]` and contains 1–16 distinct signed 32-bit integers. Arguments follow NativePortableV1 literal argument limits. There is no inferred prerequisite based on file extension or folder name, no script hooks and no shell or tool selection. Unverified names, paths, ordering or arguments belong in inert `unresolved` notes.

The v1 execution policy is prepare payload first, then offer all applicable explicitly declared installers in array order. Platform and condition filtering preserves that order. Optional installers may be explicitly declined. Attempting any installer requires user authorization on the device. Required installer failure, cancellation, denial, unavailable execution environment or an unexpected exit code prevents a successful activation. V1 does not mandate a skip/continue override. Any future override must be explicit and record an incomplete or waived result.

The optional Standard `launch` selects `associationId`, `entryPointId`, exact Association declaration `revisionId` and installed `destination`. The winning destination must come from that point's exact Artifact file. Its arguments and supported platforms come from the existing `NativePortableV1` point. A nonmatching platform has no selected launch. `workingDirectory` may explicitly override the installed directory. Otherwise the point's declared directory is interpreted relative to the installation root, or the installed executable's parent when omitted. A nonempty directory must exist. For non-Windows launch, a selected standalone File target is made executable in the installation plan, consistent with NativePortableV1 preparation. A Tree target needs its preserved executable attribute. Without this selection, ordinary Association launch declarations remain available to consumers under their existing contract. Server platform tokens specify targets, not a Wine, Proton or other runtime requirement.

A Derived manifest uses `recipePayload:true`, plus optional unresolved notes. In v1 its payload, choices, prerequisites and launch selection are absent or empty. It can carry descriptive localization. The resolver returns the existing RecipeV1 lock under `recipe` and preserves its authorization, platform and composition semantics. V1 does not expose dependency Artifacts as Release associations or provide an additional dependency selector.

## Resolve and persist

`POST /releases/{release_id}/installation/resolve` accepts `platform`, optional `selections` mapping group IDs to choice ID arrays, and optional exact Release declaration `revisionId`. If omitted, resolve the current revision. An authorized manager may send a proposed `manifest` instead of `revisionId` to preview unsaved edits using normal browser CSRF rules. That response has a null revision and is not a pinned installation. Resolution is read-only and uses one SQLite snapshot for catalog authorization, declarations, selected composition and immutable inventories. No-store responses contain no signed URLs or privileges.

A Standard plan includes version, App and Release UUIDs, Release declaration revision, platform, selections, final `files` keyed by path, sorted `directories`, ordered `prerequisites`, selected `launch` or null and unresolved notes. File references carry association and typed Artifact IDs, optional Tree file ID, SHA-256, size and executable flag. A Derived plan carries the same identity, selection and unresolved fields and embeds the Recipe lock instead of direct files or launch. Exact catalog inputs and selections produce the same plan. A historical revision can still fail if an association was removed or access revoked. Moving Recipe selectors require reuse of the embedded lock for replay. A plan reserves no bytes and cannot bypass normal download authorization or integrity verification.

A future client MUST obtain authorized Artifacts, verify Artifact identity, file lengths and digests, validate target filesystem representability, and stage files in a confined client-owned installation root independent of cached content. Assemble base and selected overlays, then handle applicable prerequisites as described above. Execute explicitly authorized binaries in the selected client platform environment with literal arguments and a scoped process API. Server metadata grants no authority for global changes, elevation, external downloads or arbitrary commands. V1 does not prescribe registry changes, system package management or runtime provisioning.

Record the exact plan and choices, operation status, prerequisite IDs and expected/actual outcomes, user consent and completion where applicable, plus activation and launch selection. A required failure leaves the prior installation intact and the new staging transaction failed. On retry, reuse previously verified bytes and the pinned plan. Reuse a prerequisite completion only if the client can verify the same prerequisite inputs and its effects in the same staged environment. An interrupted or ambiguous execution requires review and renewed authorization before another attempt. Activate only after required completion, using recoverable client-local publication. Unknown notes remain visible and never count as executed actions. The Server validates and resolves metadata only.

## GOG integration

GOG registration seeds an initial whole-Tree Release configuration in the association registration transaction when no configuration already exists and the Release declaration has a compatible object shape. Source metadata and detected content stay under the association's `gogOfflineInstaller` property. A generated Windows launch point can seed a pinned launch selection. Recognized executable prerequisite packages seed the ordered release plan. Detected file groups, unsupported setup actions and uncertain packages remain evidence. Confirmed fixed or bundled localization is descriptive and adds no language choice. A manager may explicitly seed and save a configuration from already imported GOG content without reimporting or changing published composition. Legacy clients keep reading their existing downloads and known declarations.

### Localization packaging

The optional `localization` member describes how the application ships its languages.
It does not add a version or alter content references.

| Mode | Example | Client behavior |
| --- | --- | --- |
| Fixed | `{"mode":"fixed","language":"ru-RU"}` | Display the confirmed language. Install the base payload without prompting. |
| Bundled | `{"mode":"bundled","languages":["en-US","fr-FR"]}` | Install the complete payload. Let the application handle runtime selection. |
| Selectable | `{"mode":"selectable"}` | Use the existing single-choice language option and apply the selected file mappings. There must be two or more choices with differing copy rules. |
| Unknown | `{"mode":"unknown"}` | Language support is unconfirmed. Do not infer a choice. |

Only selectable manifests use a language option group. Component groups can coexist
with any mode. Language identifiers use the same language-tag syntax as the legacy
language choices. The Server rejects duplicates ignoring case. A missing member preserves
legacy option behavior and does not assign a mode to historical revisions.
The resolution API reports the pinned revision’s localization alongside its selections.
A future client should prompt only when a language selection changes the resolved files
for the target platform. Installer UI languages and directory names do not establish
the application language.
