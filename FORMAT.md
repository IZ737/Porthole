# Porthole packed format, version 1

This is the format `porthole pack` writes and a packed app reads at launch.
It is **frozen for every 1.x release**: any Porthole 1.x launcher runs an
app packed by any Porthole 1.x, and tools outside Porthole can rely on the
layout below. A change that would break either is a new footer version and
a new major release of Porthole.

`src/payload.rs` has a byte-for-byte test of the footer and a committed
packed file (`src/testdata/packed-v1.bin`) that every release must keep
reading.

## File layout

```
[ launcher ][ payload ][ footer: 58 bytes ][ signature (optional) ]
```

- **launcher**: an unmodified `porthole` binary for the target platform,
  except as described under [Windows](#windows).
- **payload**: a zstd-compressed tar archive (see [Payload](#payload)).
- **footer**: 58 bytes, little-endian:

  | Offset | Size | Field | Value |
  | --- | --- | --- | --- |
  | 0 | 8 | `magic` | `PRTHOLE\0` (`50 52 54 48 4f 4c 45 00`) |
  | 8 | 2 | `version` | `1` |
  | 10 | 8 | `payload_offset` | File offset of the payload's first byte |
  | 18 | 8 | `payload_len` | Payload length in bytes |
  | 26 | 32 | `sha256` | SHA-256 of the payload's bytes (compressed, as stored) |

  `payload_offset + payload_len` is the footer's own offset. A file whose
  last 58 bytes (or, for a signed Windows exe, the 58 bytes before the
  signature) don't start with the magic is not a packed app: the binary
  runs as the command-line tool.

A launcher that finds a footer with a `version` other than 1 refuses to
run the app and says so; it never guesses.

### Reading the footer

1. Let `end` be the file length.
2. If the file is a PE image whose certificate table (data directory 4)
   ends exactly at `end`, set `end` to the table's offset. Up to 7 zero
   bytes of padding may sit between the footer and the table (`signtool`
   aligns the table to 8 bytes): try `end`, `end - 1`, … `end - 7`, using
   the first position where the 58 bytes before it start with the magic
   and the bytes after it, up to the table, are all zero.
3. Read the 58 bytes ending at `end` and check the magic and version.
4. Check that `payload_offset + payload_len + 58 == end`.

### Windows

`pack` makes three changes to a Windows launcher's PE image before
appending the payload. All of them are in the image itself, so a later
Authenticode signature covers them:

- The subsystem becomes GUI (2), so the app opens no console window.
- An existing certificate table entry is cleared (the signature no longer
  matches; sign the packed file instead).
- With `app.icon` set, a section named `.prsrc` is added at the end of the
  image. Its first 8 bytes hold the launcher's original resource directory
  entry (RVA and size, then 8 zero bytes); the resource directory then
  points 16 bytes into the section, at a resource tree holding the
  launcher's own resources plus `RT_ICON` / `RT_GROUP_ICON` entries made
  from the icon. That is the icon Explorer shows for the file. When the
  launcher already ends with a `.prsrc` section (packing onto a packed
  app), it is replaced, not added to.

`payload_offset` is the end of that modified image.

### macOS app bundles

`pack --macos-app` writes `[ payload ][ footer ]` to
`Contents/Resources/payload.tar.zst`, with `payload_offset` 0, and an
unmodified launcher (no footer) to `Contents/MacOS/`. A launcher without a
footer of its own that sits in a bundle's `Contents/MacOS/` reads that
file. Code signing then covers the launcher and the payload separately,
as `codesign` requires.

## Payload

A tar archive (POSIX ustar or GNU headers), compressed as one or more zstd
frames. Entries, in this order:

| Entry | Required | Contents |
| --- | --- | --- |
| `app.toml` | yes | The app's config, with `sidecar.path` or `static.root` rewritten to `app` and `app.icon` to the icon's entry name |
| `pack.toml` | yes | What `pack` recorded (see below) |
| `icon.<ext>` | no | The `app.icon` file, as given |
| `app/…` | yes | The sidecar (one file or a folder) or the static site |

`app.toml` and `pack.toml` come first, so a launcher can read them without
decompressing the rest. Entry paths are plain relative paths: a reader
rejects absolute paths, `..`, and links that point outside the archive.
Unix permission bits are kept; ownership and timestamps are not
meaningful (`pack` writes deterministic headers).

### `pack.toml`

| Key | Type | Meaning |
| --- | --- | --- |
| `porthole_version` | string | The Porthole version that packed the app |
| `devtools` | bool | Whether the web inspector is allowed (`pack --devtools`) |
| `files` | integer | Number of files under `app/` |
| `unpacked_bytes` | integer | Their total size before compression |

Readers ignore keys they don't know, so later 1.x releases may add keys.

## What may change within 1.x

- New optional keys in `pack.toml`, and new optional keys in `app.toml`.
  Existing keys keep their meaning and are never removed in 1.x.
- New optional top-level payload entries. Readers ignore entries they
  don't know.
- Compression level, tar header flavour and entry order after the first
  two, and how `pack` builds the Windows icon resources.

A launcher reads payloads written by its own and every earlier 1.x
version. `pack --target` always uses the launcher of the same version as
`pack`; with `pack --launcher`, use one at least as new as the `porthole`
that packs, since an older launcher rejects `app.toml` keys added after it.

## Not part of the format

The cache folder (`<cache dir>/porthole/<first 16 hex digits of
sha256>/`), the data folder, log names and the files Porthole keeps there
are implementation details and may change in any release.
