//! Content identity. A photo is the same photo wherever it lives, so we key it
//! by what's in it, not where it is.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK: u64 = 1024 * 1024;

/// Fast fingerprint: BLAKE3 over the file size, the first MiB and the last MiB.
/// Reads at most 2 MiB, so scanning a network share stays quick.
pub fn fingerprint(path: &Path) -> io::Result<(String, u64)> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    h.update(b"zdfp1");
    h.update(&size.to_le_bytes());

    let mut buf = vec![0u8; CHUNK as usize];
    let n = read_up_to(&mut f, &mut buf)?;
    h.update(&buf[..n]);
    if size > 2 * CHUNK {
        f.seek(SeekFrom::Start(size - CHUNK))?;
        let n = read_up_to(&mut f, &mut buf)?;
        h.update(&buf[..n]);
    } else if size > CHUNK {
        let n = read_up_to(&mut f, &mut buf)?; // the rest, no overlap
        h.update(&buf[..n]);
    }
    Ok((h.finalize().to_hex().to_string(), size))
}

/// BLAKE3 of the whole file. Used to verify copies byte for byte.
pub fn full_hash(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    io::copy(&mut f, &mut h)?;
    Ok(h.finalize().to_hex().to_string())
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_same_fingerprint_anywhere() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(d.path().join("a.jpg"), &data).unwrap();
        std::fs::create_dir(d.path().join("moved")).unwrap();
        std::fs::write(d.path().join("moved/renamed.jpg"), &data).unwrap();
        let (a, sa) = fingerprint(&d.path().join("a.jpg")).unwrap();
        let (b, _) = fingerprint(&d.path().join("moved/renamed.jpg")).unwrap();
        assert_eq!(a, b);
        assert_eq!(sa, 3_000_000);

        let mut changed = data.clone();
        *changed.last_mut().unwrap() ^= 1;
        std::fs::write(d.path().join("c.jpg"), &changed).unwrap();
        assert_ne!(fingerprint(&d.path().join("c.jpg")).unwrap().0, a);
        assert_ne!(full_hash(&d.path().join("c.jpg")).unwrap(), full_hash(&d.path().join("a.jpg")).unwrap());
    }

    #[test]
    fn small_and_medium_files() {
        let d = tempfile::tempdir().unwrap();
        for len in [0usize, 10, 1_500_000] {
            let p = d.path().join(format!("{len}.bin"));
            std::fs::write(&p, vec![1u8; len]).unwrap();
            fingerprint(&p).unwrap();
        }
    }
}
