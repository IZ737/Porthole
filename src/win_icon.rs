//! The icon Windows Explorer shows for a packed `.exe` file.
//!
//! Explorer reads a program's icon from the `RT_GROUP_ICON` / `RT_ICON`
//! resources in its PE image, not from anything the program does at run
//! time, so `pack` writes `app.icon` into the copy of the launcher. That
//! works the same when cross-packing from macOS or Linux, since it is only
//! a few header fields and one extra section.
//!
//! The launcher's own resources (if any) are kept: `pack` adds a section
//! named [`SECTION`] holding a new resource tree with the launcher's
//! entries plus the icon, and points the resource directory at it. The
//! first 8 bytes of that section remember the launcher's original resource
//! directory, so packing onto an already-packed exe replaces the icon
//! instead of stacking sections.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::icon::{self, Rgba};

/// Name of the section `pack` adds.
pub const SECTION: [u8; 8] = *b".prsrc\0\0";

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RESOURCE_DIRECTORY: usize = 2;
/// Bytes before the resource tree in [`SECTION`]: the original directory
/// entry, padded to 16.
const SECTION_PREFIX: usize = 16;
/// Sizes written, in pixels. Explorer picks the closest for each view.
const SIDES: &[u32] = &[16, 24, 32, 48, 64, 256];

/// One image of an icon, ready for an `RT_ICON` resource.
pub struct IconImage {
    pub side: u32,
    /// A 32-bit DIB for small sizes, a PNG for 256 and up (Windows Vista
    /// and later read both).
    pub bytes: Vec<u8>,
}

/// Icon images for `png` (the `app.icon` file), largest last. A
/// non-square icon is centred on a transparent square.
pub fn images(png: &[u8]) -> Result<Vec<IconImage>> {
    let img = square(icon::decode_png(png)?);
    let src = img.width;
    let mut sides: Vec<u32> = SIDES.iter().copied().filter(|s| *s <= src).collect();
    if src < 256 && !sides.contains(&src) {
        sides.push(src);
    }
    sides
        .into_iter()
        .map(|side| {
            let scaled = resize(&img, side);
            let bytes = if side >= 256 && side == src && is_rgba8_png(png, src) {
                // Already the right PNG: keep its (usually better) compression.
                png.to_vec()
            } else if side >= 256 {
                encode_png(&scaled)?
            } else {
                dib(&scaled)
            };
            Ok(IconImage { side, bytes })
        })
        .collect()
}

/// Write `images` into `pe`, a Windows launcher image (no payload, no
/// signature). `Ok(false)` if the launcher has no room for another section
/// header, which only an unusual linker would cause.
pub fn embed(pe: &mut Vec<u8>, images: &[IconImage]) -> Result<bool> {
    let h = Headers::parse(pe)?;
    let mut sections = h.sections(pe)?;

    // Drop a section an earlier `pack` added, restoring the launcher's own
    // resource directory.
    let mut original = h.resource_dir(pe);
    if let Some(last) = sections.last()
        && last.name == SECTION
    {
        let at = last.raw_offset as usize;
        if at + 8 > pe.len() {
            bail!("the launcher's {} section is damaged", ".prsrc");
        }
        original = (u32_at(pe, at), u32_at(pe, at + 4));
        let header_at = h.section_table + 40 * (sections.len() - 1);
        pe[header_at..header_at + 40].fill(0);
        pe.truncate(at);
        sections.pop();
        put_u16(pe, h.coff + 2, sections.len() as u16);
    }

    let header_at = h.section_table + 40 * sections.len();
    let first_raw = sections
        .iter()
        .map(|s| s.raw_offset)
        .filter(|o| *o > 0)
        .min()
        .unwrap_or(h.size_of_headers);
    if header_at + 40 > h.size_of_headers.min(first_raw) as usize
        || pe[header_at..header_at + 40].iter().any(|b| *b != 0)
    {
        return Ok(false);
    }

    let mut tree = if original.0 != 0 && original.1 != 0 {
        read_tree(pe, &sections, original.0, original.1)
            .context("could not read the launcher's resources")?
    } else {
        Tree::new()
    };
    tree.remove(&Key::Id(RT_ICON));
    tree.remove(&Key::Id(RT_GROUP_ICON));
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let icons = tree.entry(Key::Id(RT_ICON)).or_default();
    for (i, image) in images.iter().enumerate() {
        let id = i as u16 + 1;
        let side = if image.side >= 256 {
            0
        } else {
            image.side as u8
        };
        group.extend_from_slice(&[side, side, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes()); // planes
        group.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        group.extend_from_slice(&(image.bytes.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
        icons
            .entry(Key::Id(id))
            .or_default()
            .insert(0, image.bytes.clone());
    }
    tree.entry(Key::Id(RT_GROUP_ICON))
        .or_default()
        .entry(Key::Id(1))
        .or_default()
        .insert(0, group);

    let va = sections
        .iter()
        .map(|s| align(s.va + s.virtual_size.max(s.raw_size), h.section_alignment))
        .max()
        .unwrap_or(h.section_alignment);
    let mut data = Vec::with_capacity(SECTION_PREFIX);
    data.extend_from_slice(&original.0.to_le_bytes());
    data.extend_from_slice(&original.1.to_le_bytes());
    data.resize(SECTION_PREFIX, 0);
    let tree_bytes = write_tree(&tree, va + SECTION_PREFIX as u32);
    data.extend_from_slice(&tree_bytes);

    let raw_offset = align(pe.len() as u32, h.file_alignment);
    let raw_size = align(data.len() as u32, h.file_alignment);
    pe.resize(raw_offset as usize, 0);
    pe.extend_from_slice(&data);
    pe.resize((raw_offset + raw_size) as usize, 0);

    let mut header = [0u8; 40];
    header[..8].copy_from_slice(&SECTION);
    header[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
    header[12..16].copy_from_slice(&va.to_le_bytes());
    header[16..20].copy_from_slice(&raw_size.to_le_bytes());
    header[20..24].copy_from_slice(&raw_offset.to_le_bytes());
    // Initialized data, readable.
    header[36..40].copy_from_slice(&0x4000_0040u32.to_le_bytes());
    pe[header_at..header_at + 40].copy_from_slice(&header);
    put_u16(pe, h.coff + 2, sections.len() as u16 + 1);
    put_u32(
        pe,
        h.opt + 56,
        align(va + data.len() as u32, h.section_alignment),
    );
    // Size of initialized data.
    let init = u32_at(pe, h.opt + 8);
    put_u32(pe, h.opt + 8, init.saturating_add(raw_size));
    let dir = h.dirs + 8 * RESOURCE_DIRECTORY;
    put_u32(pe, dir, va + SECTION_PREFIX as u32);
    put_u32(pe, dir + 4, tree_bytes.len() as u32);
    // The checksum is optional for programs; signing sets it again.
    put_u32(pe, h.opt + 64, 0);
    Ok(true)
}

/// The icon images in a PE image's first `RT_GROUP_ICON`, as `(side,
/// bytes)`: what Explorer would show. For `inspect` and tests.
pub fn read_icon(pe: &[u8]) -> Result<Vec<(u32, Vec<u8>)>> {
    let h = Headers::parse(pe)?;
    let sections = h.sections(pe)?;
    let (rva, size) = h.resource_dir(pe);
    if rva == 0 || size == 0 {
        return Ok(Vec::new());
    }
    let tree = read_tree(pe, &sections, rva, size)?;
    let Some((_, langs)) = tree
        .get(&Key::Id(RT_GROUP_ICON))
        .and_then(|names| names.iter().next())
    else {
        return Ok(Vec::new());
    };
    let Some(group) = langs.values().next() else {
        return Ok(Vec::new());
    };
    let icons = tree.get(&Key::Id(RT_ICON));
    let count = u16_at(group, 4) as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let e = 6 + 14 * i;
        if e + 14 > group.len() {
            bail!("the icon group is damaged");
        }
        let side = match group[e] {
            0 => 256,
            s => u32::from(s),
        };
        let id = u16_at(group, e + 12);
        let bytes = icons
            .and_then(|i| i.get(&Key::Id(id)))
            .and_then(|l| l.values().next())
            .with_context(|| format!("icon image {id} is missing"))?;
        out.push((side, bytes.clone()));
    }
    Ok(out)
}

// ---- Images ----

/// Whether `png` is a `side`-pixel square 8-bit RGBA PNG, the kind every
/// Windows version that reads PNG icons accepts as is.
fn is_rgba8_png(png: &[u8], side: u32) -> bool {
    icon::png_size(png) == Some((side, side)) && png.get(24..26) == Some(&[8, 6])
}

fn square(img: Rgba) -> Rgba {
    if img.width == img.height {
        return img;
    }
    let side = img.width.max(img.height);
    let (dx, dy) = ((side - img.width) / 2, (side - img.height) / 2);
    let mut pixels = vec![0u8; (side * side * 4) as usize];
    for y in 0..img.height {
        let src = (y * img.width * 4) as usize;
        let dst = (((y + dy) * side + dx) * 4) as usize;
        let n = (img.width * 4) as usize;
        pixels[dst..dst + n].copy_from_slice(&img.pixels[src..src + n]);
    }
    Rgba {
        width: side,
        height: side,
        pixels,
    }
}

/// Area-average resize of a square image to `side`, in premultiplied
/// alpha so transparent edges don't darken.
fn resize(img: &Rgba, side: u32) -> Rgba {
    let src = img.width;
    if side == src {
        return Rgba {
            width: side,
            height: side,
            pixels: img.pixels.clone(),
        };
    }
    let scale = src as f64 / side as f64;
    // For each output column/row: the source pixels it covers, with weights.
    let spans: Vec<Vec<(usize, f64)>> = (0..side)
        .map(|o| {
            let (start, end) = (o as f64 * scale, (o + 1) as f64 * scale);
            let mut v = Vec::new();
            let mut i = start.floor() as usize;
            while (i as f64) < end && i < src as usize {
                let w = (end.min(i as f64 + 1.0) - start.max(i as f64)).max(0.0);
                if w > 0.0 {
                    v.push((i, w));
                }
                i += 1;
            }
            v
        })
        .collect();
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for ys in &spans {
        for xs in &spans {
            let mut acc = [0f64; 4];
            let mut total = 0.0;
            for &(y, wy) in ys {
                for &(x, wx) in xs {
                    let w = wx * wy;
                    let p = &img.pixels[(y * src as usize + x) * 4..][..4];
                    let a = p[3] as f64 / 255.0;
                    acc[0] += p[0] as f64 * a * w;
                    acc[1] += p[1] as f64 * a * w;
                    acc[2] += p[2] as f64 * a * w;
                    acc[3] += a * w;
                    total += w;
                }
            }
            let alpha = acc[3] / total;
            let un = |c: f64| {
                if acc[3] > 0.0 {
                    (c / acc[3]).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            pixels.extend_from_slice(&[
                un(acc[0]),
                un(acc[1]),
                un(acc[2]),
                (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
            ]);
        }
    }
    Rgba {
        width: side,
        height: side,
        pixels,
    }
}

fn encode_png(img: &Rgba) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&img.pixels)?;
    writer.finish()?;
    Ok(out)
}

/// The icon-resource form of a bitmap: a `BITMAPINFOHEADER` with double
/// height, bottom-up BGRA rows, then the 1-bit AND mask.
fn dib(img: &Rgba) -> Vec<u8> {
    let side = img.width as usize;
    let mask_row = side.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + side * side * 4 + mask_row * side);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    out.extend_from_slice(&(2 * side as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((side * side * 4 + mask_row * side) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for y in (0..side).rev() {
        for x in 0..side {
            let p = &img.pixels[(y * side + x) * 4..][..4];
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    for y in (0..side).rev() {
        let mut row = vec![0u8; mask_row];
        for x in 0..side {
            if img.pixels[(y * side + x) * 4 + 3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

// ---- PE headers ----

struct Headers {
    coff: usize,
    opt: usize,
    dirs: usize,
    dir_count: usize,
    section_table: usize,
    section_alignment: u32,
    file_alignment: u32,
    size_of_headers: u32,
}

struct Section {
    name: [u8; 8],
    virtual_size: u32,
    va: u32,
    raw_size: u32,
    raw_offset: u32,
}

impl Headers {
    fn parse(pe: &[u8]) -> Result<Self> {
        if pe.len() < 0x40 || &pe[..2] != b"MZ" {
            bail!("not a Windows executable");
        }
        let at = u32_at(pe, 0x3c) as usize;
        if at.checked_add(24 + 240).is_none_or(|end| end > pe.len()) || &pe[at..at + 4] != b"PE\0\0"
        {
            bail!("not a Windows executable");
        }
        let coff = at + 4;
        let opt = at + 24;
        let opt_size = u16_at(pe, coff + 16) as usize;
        let dirs = match u16_at(pe, opt) {
            0x10b => opt + 96,
            0x20b => opt + 112,
            _ => bail!("unknown PE optional header"),
        };
        let h = Headers {
            coff,
            opt,
            dirs,
            dir_count: u32_at(pe, dirs - 4) as usize,
            section_table: opt + opt_size,
            section_alignment: u32_at(pe, opt + 32).max(1),
            file_alignment: u32_at(pe, opt + 36).max(1),
            size_of_headers: u32_at(pe, opt + 60),
        };
        if h.dir_count <= RESOURCE_DIRECTORY || h.size_of_headers as usize > pe.len() {
            bail!("the launcher's PE header is damaged");
        }
        Ok(h)
    }

    fn sections(&self, pe: &[u8]) -> Result<Vec<Section>> {
        let count = u16_at(pe, self.coff + 2) as usize;
        if self.section_table + 40 * count > pe.len() {
            bail!("the launcher's section table is damaged");
        }
        Ok((0..count)
            .map(|i| {
                let s = self.section_table + 40 * i;
                Section {
                    name: pe[s..s + 8].try_into().unwrap(),
                    virtual_size: u32_at(pe, s + 8),
                    va: u32_at(pe, s + 12),
                    raw_size: u32_at(pe, s + 16),
                    raw_offset: u32_at(pe, s + 20),
                }
            })
            .collect())
    }

    fn resource_dir(&self, pe: &[u8]) -> (u32, u32) {
        let at = self.dirs + 8 * RESOURCE_DIRECTORY;
        (u32_at(pe, at), u32_at(pe, at + 4))
    }
}

/// File offset of `rva`, and how many bytes the section has from there.
fn file_offset(sections: &[Section], rva: u32) -> Option<(usize, usize)> {
    sections.iter().find_map(|s| {
        let size = s.virtual_size.max(s.raw_size);
        (rva >= s.va && rva < s.va + size).then(|| {
            let into = rva - s.va;
            (
                (s.raw_offset + into) as usize,
                s.raw_size.saturating_sub(into) as usize,
            )
        })
    })
}

// ---- Resource trees ----

/// A resource type or name: names sort before ids, as Windows requires.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Name(Vec<u16>),
    Id(u16),
}

/// type → name → language → data.
type Tree = BTreeMap<Key, BTreeMap<Key, BTreeMap<u16, Vec<u8>>>>;

fn read_tree(pe: &[u8], sections: &[Section], rva: u32, size: u32) -> Result<Tree> {
    let (start, avail) = file_offset(sections, rva).context("resource directory not mapped")?;
    let len = (size as usize).min(avail);
    if start + len > pe.len() {
        bail!("resource directory outside the file");
    }
    let rsrc = &pe[start..start + len];
    let get = |at: usize, n: usize| -> Result<&[u8]> {
        rsrc.get(at..at + n).context("resource tree is damaged")
    };
    let entries = |at: usize| -> Result<Vec<(Key, u32)>> {
        let head = get(at, 16)?;
        let n = u16_at(head, 12) as usize + u16_at(head, 14) as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n.min(4096) {
            let e = get(at + 16 + 8 * i, 8)?;
            let (name, target) = (u32_at(e, 0), u32_at(e, 4));
            let key = if name & 0x8000_0000 != 0 {
                let s = (name & 0x7fff_ffff) as usize;
                let chars = u16_at(get(s, 2)?, 0) as usize;
                let raw = get(s + 2, 2 * chars)?;
                Key::Name((0..chars).map(|c| u16_at(raw, 2 * c)).collect())
            } else {
                Key::Id(name as u16)
            };
            out.push((key, target));
        }
        Ok(out)
    };
    let subdir = |target: u32| -> Result<usize> {
        if target & 0x8000_0000 == 0 {
            bail!("resource tree is damaged");
        }
        Ok((target & 0x7fff_ffff) as usize)
    };
    let mut tree = Tree::new();
    for (ty, t) in entries(0)? {
        for (name, n) in entries(subdir(t)?)? {
            for (lang, l) in entries(subdir(n)?)? {
                let Key::Id(lang) = lang else {
                    bail!("resource tree is damaged");
                };
                let entry = get(l as usize, 16)?;
                let (data_rva, data_size) = (u32_at(entry, 0), u32_at(entry, 4) as usize);
                let (at, avail) =
                    file_offset(sections, data_rva).context("resource data not mapped")?;
                if data_size > avail || at + data_size > pe.len() {
                    bail!("resource data outside the file");
                }
                tree.entry(ty.clone())
                    .or_default()
                    .entry(name.clone())
                    .or_default()
                    .insert(lang, pe[at..at + data_size].to_vec());
            }
        }
    }
    Ok(tree)
}

/// Serialize `tree` for a resource directory at `rva`.
fn write_tree(tree: &Tree, rva: u32) -> Vec<u8> {
    let dir_size = |n: usize| 16 + 8 * n;
    // Layout: root, type directories, name directories, data entries,
    // strings, data.
    let mut offset = dir_size(tree.len());
    let mut type_dirs = Vec::new();
    for names in tree.values() {
        type_dirs.push(offset);
        offset += dir_size(names.len());
    }
    let mut name_dirs = Vec::new();
    for names in tree.values() {
        for langs in names.values() {
            name_dirs.push(offset);
            offset += dir_size(langs.len());
        }
    }
    let leaves: usize = tree
        .values()
        .flat_map(|n| n.values())
        .map(|l| l.len())
        .sum();
    let entries_at = offset;
    offset += 16 * leaves;
    let mut strings = BTreeMap::new();
    for (ty, names) in tree {
        for key in std::iter::once(ty).chain(names.keys()) {
            if let Key::Name(s) = key
                && !strings.contains_key(s)
            {
                strings.insert(s.clone(), offset);
                offset += 2 + 2 * s.len();
            }
        }
    }
    offset = offset.next_multiple_of(8);
    let mut out = vec![0u8; offset];

    let key_field = |key: &Key| match key {
        Key::Name(s) => 0x8000_0000 | strings[s] as u32,
        Key::Id(id) => u32::from(*id),
    };
    let dir_header = |out: &mut [u8], at: usize, keys: &mut dyn Iterator<Item = &Key>| {
        let (mut named, mut ids) = (0u16, 0u16);
        for k in keys {
            match k {
                Key::Name(_) => named += 1,
                Key::Id(_) => ids += 1,
            }
        }
        put_u16(out, at + 12, named);
        put_u16(out, at + 14, ids);
    };

    dir_header(&mut out, 0, &mut tree.keys());
    let (mut ni, mut leaf) = (0, 0);
    for (ti, (ty, names)) in tree.iter().enumerate() {
        put_u32(&mut out, 16 + 8 * ti, key_field(ty));
        put_u32(
            &mut out,
            16 + 8 * ti + 4,
            0x8000_0000 | type_dirs[ti] as u32,
        );
        let td = type_dirs[ti];
        dir_header(&mut out, td, &mut names.keys());
        for (j, (name, langs)) in names.iter().enumerate() {
            let nd = name_dirs[ni];
            ni += 1;
            put_u32(&mut out, td + 16 + 8 * j, key_field(name));
            put_u32(&mut out, td + 16 + 8 * j + 4, 0x8000_0000 | nd as u32);
            put_u16(&mut out, nd + 14, langs.len() as u16);
            for (k, (lang, data)) in langs.iter().enumerate() {
                let entry = entries_at + 16 * leaf;
                leaf += 1;
                put_u32(&mut out, nd + 16 + 8 * k, u32::from(*lang));
                put_u32(&mut out, nd + 16 + 8 * k + 4, entry as u32);
                let data_at = out.len();
                out.extend_from_slice(data);
                out.resize(out.len().next_multiple_of(8), 0);
                put_u32(&mut out, entry, rva + data_at as u32);
                put_u32(&mut out, entry + 4, data.len() as u32);
            }
        }
    }
    for (s, at) in &strings {
        put_u16(&mut out, *at, s.len() as u16);
        for (i, c) in s.iter().enumerate() {
            put_u16(&mut out, at + 2 + 2 * i, *c);
        }
    }
    out
}

fn align(v: u32, to: u32) -> u32 {
    v.div_ceil(to) * to
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn put_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A PE32+ image laid out like a linker would: headers padded to
    /// 0x400, then `.text` and `.rsrc` (holding one `RT_MANIFEST`).
    pub fn sectioned_pe() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        put_u16(&mut b, coff, 0x8664);
        put_u16(&mut b, coff + 2, 2);
        put_u16(&mut b, coff + 16, 240);
        let opt = 0x80 + 24;
        put_u16(&mut b, opt, 0x20b);
        put_u32(&mut b, opt + 32, 0x1000);
        put_u32(&mut b, opt + 36, 0x200);
        put_u32(&mut b, opt + 56, 0x3000);
        put_u32(&mut b, opt + 60, 0x400);
        put_u16(&mut b, opt + 68, 3);
        put_u32(&mut b, opt + 108, 16);

        let mut manifest = Tree::new();
        manifest
            .entry(Key::Id(24))
            .or_default()
            .entry(Key::Id(1))
            .or_default()
            .insert(0x409, b"<assembly/>".to_vec());
        manifest
            .entry(Key::Name("CUSTOM".encode_utf16().collect()))
            .or_default()
            .entry(Key::Name("THING".encode_utf16().collect()))
            .or_default()
            .insert(0, b"hello".to_vec());
        let rsrc = write_tree(&manifest, 0x2000);

        let table = opt + 240;
        let text = [b".text\0\0\0", b".rsrc\0\0\0"];
        let sizes = [0x200u32, align(rsrc.len() as u32, 0x200)];
        for i in 0..2 {
            let s = table + 40 * i;
            b[s..s + 8].copy_from_slice(text[i]);
            put_u32(&mut b, s + 8, if i == 0 { 0x10 } else { rsrc.len() as u32 });
            put_u32(&mut b, s + 12, 0x1000 * (i as u32 + 1));
            put_u32(&mut b, s + 16, sizes[i]);
            put_u32(&mut b, s + 20, 0x400 + 0x200 * i as u32);
        }
        b.resize(0x600, 0xcc);
        b.extend_from_slice(&rsrc);
        b.resize(0x600 + sizes[1] as usize, 0);
        let dir = opt + 112 + 16;
        put_u32(&mut b, dir, 0x2000);
        put_u32(&mut b, dir + 4, rsrc.len() as u32);
        b
    }

    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer
            .write_image_data(&[255, 0, 0, 255].repeat((w * h) as usize))
            .unwrap();
        drop(writer);
        out
    }

    #[test]
    fn makes_every_size_up_to_the_source() {
        let imgs = images(&red_png(512, 512)).unwrap();
        let sides: Vec<u32> = imgs.iter().map(|i| i.side).collect();
        assert_eq!(sides, [16, 24, 32, 48, 64, 256]);
        let big = icon::decode_png(&imgs[5].bytes).unwrap();
        assert_eq!((big.width, big.height), (256, 256));
        assert_eq!(&big.pixels[..4], &[255, 0, 0, 255]);
        // 16 px DIB: header, 16x16 BGRA, 16 rows of 4-byte mask.
        assert_eq!(imgs[0].bytes.len(), 40 + 16 * 16 * 4 + 16 * 4);
        assert_eq!(&imgs[0].bytes[40..44], &[0, 0, 255, 255]);

        let small: Vec<u32> = images(&red_png(40, 20))
            .unwrap()
            .iter()
            .map(|i| i.side)
            .collect();
        assert_eq!(small, [16, 24, 32, 40]);
    }

    #[test]
    fn resize_keeps_transparent_edges_clean() {
        // Left half opaque white, right half transparent black.
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels.extend_from_slice(&[255, 255, 255, 255, 255, 255, 255, 255]);
            pixels.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        }
        let img = Rgba {
            width: 4,
            height: 4,
            pixels,
        };
        let out = resize(&img, 1);
        assert_eq!(out.pixels, [255, 255, 255, 128]);
    }

    #[test]
    fn embeds_and_replaces_the_icon_keeping_other_resources() {
        let mut pe = sectioned_pe();
        let before = pe.clone();
        assert!(read_icon(&pe).unwrap().is_empty());
        assert!(embed(&mut pe, &images(&red_png(48, 48)).unwrap()).unwrap());
        let icon = read_icon(&pe).unwrap();
        assert_eq!(
            icon.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            [16, 24, 32, 48]
        );
        assert_eq!(pe.len() % 0x200, 0);
        assert_eq!(&pe[..0x80], &before[..0x80]);

        // The launcher's own resources survive.
        let h = Headers::parse(&pe).unwrap();
        let sections = h.sections(&pe).unwrap();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[2].name, SECTION);
        assert_eq!(sections[2].va, 0x3000);
        assert_eq!(
            u32_at(&pe, h.opt + 56),
            align(0x3000 + sections[2].virtual_size, 0x1000)
        );
        let (rva, size) = h.resource_dir(&pe);
        let tree = read_tree(&pe, &sections, rva, size).unwrap();
        assert_eq!(tree[&Key::Id(24)][&Key::Id(1)][&0x409], b"<assembly/>");
        let custom = Key::Name("CUSTOM".encode_utf16().collect());
        let thing = Key::Name("THING".encode_utf16().collect());
        assert_eq!(tree[&custom][&thing][&0], b"hello");

        // Packing again (onto a packed launcher) replaces the section.
        assert!(embed(&mut pe, &images(&red_png(256, 256)).unwrap()).unwrap());
        let h = Headers::parse(&pe).unwrap();
        assert_eq!(h.sections(&pe).unwrap().len(), 3);
        let sides: Vec<u32> = read_icon(&pe).unwrap().iter().map(|(s, _)| *s).collect();
        assert_eq!(sides, [16, 24, 32, 48, 64, 256]);
        let tree = read_tree(
            &pe,
            &h.sections(&pe).unwrap(),
            h.resource_dir(&pe).0,
            h.resource_dir(&pe).1,
        )
        .unwrap();
        assert_eq!(tree[&Key::Id(24)][&Key::Id(1)][&0x409], b"<assembly/>");
    }

    #[test]
    fn launchers_without_room_are_left_alone() {
        // Headers end right after the section table.
        let mut pe = crate::exe::tests::fake_pe(2, 0);
        let len = pe.len();
        let opt = 0x80 + 24;
        put_u32(&mut pe, opt + 60, len as u32);
        put_u16(&mut pe, 0x84 + 16, 240);
        assert!(!embed(&mut pe, &images(&red_png(16, 16)).unwrap()).unwrap());
        assert_eq!(pe.len(), len);
    }
}
