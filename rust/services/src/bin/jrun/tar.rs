//! App packages are tar archives (ustar, or GNU tar's default format with
//! short names), uncompressed: the jars inside are already compressed.
//!
//! The reader is strict, because `jrun install` runs as root on files from
//! anywhere: only regular files and directories, only relative paths without
//! `..`, no duplicates, and size limits. Anything else is an error, never
//! skipped silently.

use std::collections::BTreeMap;

pub const MAX_TOTAL: u64 = 256 << 20;
const BLOCK: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Dir,
    File { data: Vec<u8>, executable: bool },
}

/// Path (relative, '/'-separated, no trailing '/') -> item, sorted.
pub type Entries = BTreeMap<String, Item>;

fn octal(field: &[u8], what: &str) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(format!("{what}: base-256 numbers (files over 8 GiB) aren't supported"));
    }
    let s: String = field.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| format!("{what}: bad number {s:?}"))
}

fn cstr(field: &[u8]) -> Result<String, String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8(field[..end].to_vec()).map_err(|_| "file name isn't UTF-8".to_string())
}

/// Normalizes an archive path; Err for anything that could escape the
/// directory it's extracted into. Ok(None) for the archive root ("./").
pub fn clean_path(raw: &str) -> Result<Option<String>, String> {
    if raw.starts_with('/') {
        return Err(format!("{raw:?}: absolute paths aren't allowed"));
    }
    let mut parts = Vec::new();
    for p in raw.split('/') {
        match p {
            "" | "." => {}
            ".." => return Err(format!("{raw:?}: \"..\" isn't allowed")),
            p if p.chars().any(|c| c.is_control() || c == '\\') => {
                return Err(format!("{raw:?}: control characters and backslashes aren't allowed"))
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join("/")))
}

pub fn read(data: &[u8]) -> Result<Entries, String> {
    if data.starts_with(&[0x1f, 0x8b]) || data.starts_with(b"BZh") || data.starts_with(&[0xfd, b'7', b'z', b'X', b'Z']) {
        return Err("not a tar archive but a compressed one: app packages are uncompressed tar files".into());
    }
    let mut out = Entries::new();
    let mut total = 0u64;
    let mut pos = 0usize;
    loop {
        if pos + BLOCK > data.len() {
            if pos == data.len() && !out.is_empty() {
                break; // no end-of-archive blocks: accept, as tar does
            }
            return Err("truncated archive".into());
        }
        let h = &data[pos..pos + BLOCK];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let stored = octal(&h[148..156], "header checksum")?;
        let sum: u64 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
        if stored != sum {
            return Err(format!("bad header checksum at byte {pos}: not a tar archive?"));
        }
        if &h[257..262] != b"ustar" {
            return Err("not a ustar or GNU tar archive (old v7 format?)".into());
        }
        let size = octal(&h[124..136], "size")?;
        let mode = octal(&h[100..108], "mode")?;
        let mut name = cstr(&h[0..100])?;
        if &h[257..263] == b"ustar\0" {
            let prefix = cstr(&h[345..500])?;
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let typeflag = h[156];
        pos += BLOCK;
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        if size > MAX_TOTAL || pos as u64 + padded > data.len() as u64 {
            return Err(format!("{name:?}: truncated or too large"));
        }
        let body = &data[pos..pos + size as usize];
        pos += padded as usize;
        let item = match typeflag {
            b'0' | 0 => {
                total += size;
                if total > MAX_TOTAL {
                    return Err(format!("package contents are over {} MiB", MAX_TOTAL >> 20));
                }
                Item::File { data: body.to_vec(), executable: mode & 0o111 != 0 }
            }
            b'5' => Item::Dir,
            b'1' | b'2' => return Err(format!("{name:?}: links aren't allowed in app packages")),
            b'x' | b'g' | b'L' | b'K' => {
                return Err(format!("{name:?}: long names and pax headers aren't supported; keep paths under 100 characters or use `tar --format=ustar`"))
            }
            t => return Err(format!("{name:?}: entry type {:?} isn't allowed (only files and directories)", t as char)),
        };
        let Some(path) = clean_path(&name)? else {
            if item == Item::Dir {
                continue;
            }
            return Err(format!("{name:?}: a file with no name"));
        };
        // A file's parents must not be files.
        let mut parent = path.as_str();
        while let Some((p, _)) = parent.rsplit_once('/') {
            match out.get(p) {
                Some(Item::File { .. }) => return Err(format!("{path:?}: {p:?} is a file")),
                Some(Item::Dir) => {}
                None => {
                    out.insert(p.to_string(), Item::Dir);
                }
            }
            parent = p;
        }
        match (out.get(&path), &item) {
            (None, _) => {
                out.insert(path, item);
            }
            (Some(Item::Dir), Item::Dir) => {}
            _ => return Err(format!("{path:?} appears twice")),
        }
    }
    Ok(out)
}

fn put_octal(field: &mut [u8], n: u64) {
    let s = format!("{:0width$o}\0", n, width = field.len() - 1);
    field.copy_from_slice(s.as_bytes());
}

/// Writes a ustar archive: owner root, fixed time, sorted, so the same
/// contents always give the same bytes.
pub fn write(entries: &Entries, mtime: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for (path, item) in entries {
        let mut h = [0u8; BLOCK];
        let name = if matches!(item, Item::Dir) { format!("{path}/") } else { path.clone() };
        let (prefix, base) = if name.len() <= 100 {
            ("", name.as_str())
        } else {
            // Split at a '/' so that the prefix is <= 155 and the rest <= 100.
            let cut = name[..name.len().min(156)]
                .rmatch_indices('/')
                .map(|(i, _)| i)
                .find(|&i| name.len() - i - 1 <= 100 && i > 0)
                .ok_or_else(|| format!("{path:?}: name too long for a tar archive"))?;
            (&name[..cut], &name[cut + 1..])
        };
        h[..base.len()].copy_from_slice(base.as_bytes());
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        let (mode, size, flag) = match item {
            Item::Dir => (0o755, 0, b'5'),
            Item::File { data, executable } => (if *executable { 0o755 } else { 0o644 }, data.len() as u64, b'0'),
        };
        put_octal(&mut h[100..108], mode);
        put_octal(&mut h[108..116], 0);
        put_octal(&mut h[116..124], 0);
        put_octal(&mut h[124..136], size);
        put_octal(&mut h[136..148], mtime);
        h[156] = flag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[265..269].copy_from_slice(b"root");
        h[297..301].copy_from_slice(b"root");
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| b as u64).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        out.extend_from_slice(&h);
        if let Item::File { data, .. } = item {
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(s: &str) -> Item {
        Item::File { data: s.as_bytes().to_vec(), executable: false }
    }

    #[test]
    fn round_trip() {
        let mut e = Entries::new();
        e.insert("app.toml".into(), file("id = \"x\"\n"));
        e.insert("lib".into(), Item::Dir);
        e.insert("lib/a.jar".into(), Item::File { data: vec![1; 1000], executable: true });
        let long = format!("{}/{}", "d".repeat(120), "f".repeat(90));
        e.insert("d".repeat(120), Item::Dir);
        e.insert(long.clone(), file("deep"));
        let bytes = write(&e, 0).unwrap();
        assert_eq!(read(&bytes).unwrap(), e);
    }

    #[test]
    fn paths() {
        assert_eq!(clean_path("./a//b/").unwrap(), Some("a/b".into()));
        assert_eq!(clean_path("./").unwrap(), None);
        assert!(clean_path("/etc/passwd").is_err());
        assert!(clean_path("a/../../x").is_err());
        assert!(clean_path("a\nb").is_err());
    }

    #[test]
    fn rejects_traversal_and_links() {
        let mut e = Entries::new();
        e.insert("ok".into(), file("x"));
        let mut bytes = write(&e, 0).unwrap();
        // rename "ok" to "../x" and fix the checksum
        bytes[..4].copy_from_slice(b"../x");
        let sum: u64 = bytes[..512].iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
        bytes[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(read(&bytes).unwrap_err().contains(".."));
        bytes[..4].copy_from_slice(b"okay");
        bytes[156] = b'2';
        let sum: u64 = bytes[..512].iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
        bytes[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(read(&bytes).unwrap_err().contains("links"));
        assert!(read(b"garbage").is_err());
        assert!(read(&[0u8; 2048]).unwrap().is_empty());
    }
}
