# Canonical Tree Identity v1

This is the normative identity format for a directory Artifact. A file Artifact is a different content identity type. The logical tree, including portable attributes, is the input; source root name, absolute path, enumeration order, host OS, locale, ingestion method, App, Release and declarations are not inputs.

## Binary stream

All integers are unsigned and little-endian. Concatenate fields exactly as listed, without terminators, padding or host structure bytes:

| Field | Encoding |
| --- | --- |
| Magic | literal ASCII `APPDEPOT-TREE` (13 bytes) |
| Version | uint16, value `1` |
| Entry count | uint64 |
| Each entry, in canonical order | type byte (`01` file, `02` directory); uint32 path byte length; exactly that many UTF-8 path bytes |
| Additional fields for a file only | uint64 file size; 32 raw bytes of SHA-256 over the file bytes; executable byte (`00` or `01`) |

The root directory is implicit and has no record. Every other directory has a record, including an empty directory. A directory record has no additional fields. No timestamps, ownership, full permission modes, extended attributes or platform metadata are serialized.

Sort complete relative paths lexicographically by **unsigned raw UTF-8 bytes**. Do not apply locale collation, case folding, Unicode normalization or filesystem order. The sort key includes `/` bytes in the path. Encode each path exactly once.

## Validity and identifiers

A path is a nonempty sequence of nonempty UTF-8 components separated only by ASCII `/`. Decode UTF-8 strictly; retain the original code-point sequence. Reject leading or trailing `/`, repeated `/`, `.` or `..` components, NUL, backslash, a leading Windows drive prefix (`[A-Za-z]:`), invalid UTF-8, absolute or escaping paths and any other ambiguous representation. Do not rename, normalize or repair a rejected path. An entry's path byte length must fit uint32. Each non-root parent must have an explicit directory record; a file cannot be a parent. Reject duplicate paths, file/directory collisions, unsupported entry types, noncanonical order, record-count mismatch, truncated fields, invalid executable bytes and trailing bytes. A file size must fit uint64 and match the verified file bytes represented by its SHA-256. The tree must contain at least one regular file. Unsupported source entries include symbolic links, hard-link entries, devices, sockets, FIFOs and other special entries. Directory permissions do not participate; the only portable permission attribute is the file executable flag.

Verify the file byte size and SHA-256 and all tree validity conditions before accepting an identity. Compute `H = SHA-256(canonical stream)`. The external tree identifier is exactly `adtree:v1:sha256:` followed by 64 **lowercase** hexadecimal digits of `H`; no prefix or alternate spelling is accepted. This typed, versioned identifier is distinct from any bare file SHA-256 or CAS digest identifier. The magic and version are part of the hashed stream and separate this hash domain. SHA-256 collision resistance is the basis of content identity, not a mathematical guarantee that distinct bytes have distinct digests.

A valid logical tree may contain names that collide on a case-insensitive filesystem or cannot be represented on a destination filesystem. That does not invalidate its identity. A client must detect destination incompatibility, including path collisions, before materializing it; it must not rename entries.

## Bare and ZIP ingestion

Both methods produce the same logical entries and canonical stream. A bare directory's selected root name is omitted; the ingested file's verified size, digest and portable executable flag are used. On a POSIX source, any of the three execute permission bits sets that flag to `01`; otherwise it is `00`. A platform without a native executable bit must provide the flag explicitly or use `00`; it must not infer it from extensions. ZIP is transport only. The MVP accepts ZIP Store and Deflate. ZIP member names are paths relative to the selected Artifact root; no wrapper directory is stripped automatically. An omitted nonempty parent directory is synthesized as a directory entry, while an explicit empty directory is retained. Duplicate or conflicting members are rejected.

For ZIP, a name ending in `/` denotes a directory; other names denote files. Unix-origin means the upper byte of the ZIP “version made by” field is `03`; its mode is the upper 16 bits of the ZIP external attributes. If mode type bits `0170000` (octal) are nonzero, they must agree with the name: `0100000` for a file or `0040000` for a directory; other types and contradictions are rejected. A DOS directory bit (`0x10` in the low external-attribute byte), if set, must also agree. For a regular file with Unix-origin mode, set executable to `01` if `(mode & 000111) != 0` (octal), otherwise `00`. For other origin systems or zero Unix mode, set it to `00`; no extension-based inference is allowed. ZIP timestamps, ownership and other attributes are ignored. Names with ZIP general-purpose flag bit 11 set must contain valid UTF-8. Without that flag, only ASCII member-name bytes are accepted; non-ASCII names without unambiguous UTF-8 encoding are rejected. ZIP path spelling is preserved after decoding, with no Unicode normalization. ZIP directory members carry no executable attribute. This mapping permits bare and ZIP imports to agree when their logical file flags agree; publishers needing executable files must encode the Unix execute bits in ZIP.

## Test vectors

All file contents below are literal bytes; `A` is `41`, `B` is `42`. Hexadecimal streams are complete, including header. `V7` and `V8` intentionally repeat earlier bytes and identities.

### V1

One ASCII file: `a.txt` = `A`, executable `00`.

`4150504445504f542d54524545010001000000000000000105000000612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd00`

SHA-256: `49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1`
Identity: `adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1`

### V2

Nested directory `d/`; `b.txt` = `B`, `d/a.txt` = `A`, `d/z.bin` = `00ff`; all flags `00`.

`4150504445504f542d54524545010004000000000000000105000000622e7478740100000000000000df7e70e5021544f4834bbee64a9e3789febc4be81470df629cad6ddb03320a5c000201000000640107000000642f612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd000107000000642f7a2e62696e020000000000000006eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b800`

SHA-256: `8270d0fb4808f9cbdc391f9c27eb83b78944716a7f838a2aab348db1e2f45645`
Identity: `adtree:v1:sha256:8270d0fb4808f9cbdc391f9c27eb83b78944716a7f838a2aab348db1e2f45645`

### V3

`a.txt` = `A`, plus empty `empty/` and `empty/nested/`.

`4150504445504f542d54524545010003000000000000000105000000612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd000205000000656d707479020c000000656d7074792f6e6573746564`

SHA-256: `de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f`
Identity: `adtree:v1:sha256:de5249a4b310ed62cbdd7fe30ed926cba8a974e3bba838203445ee0f9a93398f`

### V4

`a.txt` = `A`, executable `01` (compare V1).

`4150504445504f542d54524545010001000000000000000105000000612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd01`

SHA-256: `082f91792688612638863968a8f5923a0d78672c1c705f0c15c769c4146c2c65`
Identity: `adtree:v1:sha256:082f91792688612638863968a8f5923a0d78672c1c705f0c15c769c4146c2c65`

### V5

Case-distinct `A.txt` = `A` and `a.txt` = `A`, both flags `00`.

`4150504445504f542d54524545010002000000000000000105000000412e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd000105000000612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd00`

SHA-256: `d0f7a0fbb72d807764a41fbf871e29d1f42eb5f663af6d9c19a10e61a98899db`
Identity: `adtree:v1:sha256:d0f7a0fbb72d807764a41fbf871e29d1f42eb5f663af6d9c19a10e61a98899db`

### V6

`cafe\u0301.txt` (decomposed `e` + U+0301) and `caf\u00e9.txt` (U+00E9), both = `A`, flags `00`; the two Unicode sequences remain distinct.

`4150504445504f542d5452454501000200000000000000010a00000063616665cc812e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd000109000000636166c3a92e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd00`

SHA-256: `c1f6b66d1bb2cd88636493b65469c2eee6d01f9ba3c539e6936e75679d5a6f7b`
Identity: `adtree:v1:sha256:c1f6b66d1bb2cd88636493b65469c2eee6d01f9ba3c539e6936e75679d5a6f7b`

### V7

Two source roots named `alpha` and `beta`, each containing V1; both yield these same bytes.

`4150504445504f542d54524545010001000000000000000105000000612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd00`

SHA-256: `49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1`
Identity: `adtree:v1:sha256:49559a44d0b66acf4aad338e65a045879d8e575fd6e6c595439967a2aa8893d1`

### V8

V2 logical tree ingested bare and from ZIP Store/Deflate with matching flags; both yield these same bytes.

`4150504445504f542d54524545010004000000000000000105000000622e7478740100000000000000df7e70e5021544f4834bbee64a9e3789febc4be81470df629cad6ddb03320a5c000201000000640107000000642f612e7478740100000000000000559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd000107000000642f7a2e62696e020000000000000006eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b800`

SHA-256: `8270d0fb4808f9cbdc391f9c27eb83b78944716a7f838a2aab348db1e2f45645`
Identity: `adtree:v1:sha256:8270d0fb4808f9cbdc391f9c27eb83b78944716a7f838a2aab348db1e2f45645`

### V9

`a.txt` = `B`, flag `00` (compare V1: changed file bytes).

`4150504445504f542d54524545010001000000000000000105000000612e7478740100000000000000df7e70e5021544f4834bbee64a9e3789febc4be81470df629cad6ddb03320a5c00`

SHA-256: `8dc2adf5183190576bf7c0142cfc8fdb6f70fc2a01c115a33e9012b10213e42b`
Identity: `adtree:v1:sha256:8dc2adf5183190576bf7c0142cfc8fdb6f70fc2a01c115a33e9012b10213e42b`

### Negative vectors

Each case must be rejected before accepting an Artifact identity:

| Case | Input | Exact reason |
| --- | --- | --- |
| N1 | V1 stream with entry count bytes (offsets 15–22) changed from `0100000000000000` to `0200000000000000` | Count declares two records, but only one exists. |
| N2 | V1 stream with type byte at offset 23 changed from `01` to `03` | Unsupported entry type. |
| N3 | V1 stream with executable byte at offset 73 changed from `00` to `02` | Executable value is outside `00`/`01`. |
| N4 | V1 stream followed by `00` | Trailing byte. |
| N5 | V1 stream with path bytes (offsets 28–32) changed to `2e2e2f6162` | `../ab` contains parent traversal. |
| N6 | V1 stream with path bytes changed to `61ff747874` | Invalid UTF-8. |
| N7 | V3 records reordered so `empty/nested` precedes `empty` | Entries are not sorted by raw path bytes. |
| N8 | Logical entries: file `d/a.txt`, no `d` directory, plus root file `a.txt` | Missing explicit non-root parent directory. |
| N9 | Logical entries: file `d`, file `d/a.txt` | File acts as a parent. |
| N10 | Logical entries: two files at `a.txt` | Duplicate path. |
| N11 | Logical entries: directory `a.txt` and file `a.txt` | File/directory collision. |
| N12 | Logical entries: only empty directory `empty` | No regular file. |
| N13 | Logical entries: symbolic link `link` plus file `a.txt` | Unsupported filesystem entry. |
| N14 | ZIP file member `a.txt` with Unix execute bits set, imported as non-executable | Incorrect ZIP-to-tree executable mapping. |
