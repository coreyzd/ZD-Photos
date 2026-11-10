//! Content identity. A photo is the same photo wherever it lives, so we key it
//! by what's in it, not where it is.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK: u64 = 1024 * 1024;

/// Fast fingerprint: BLAKE3 over the file size, the first MiB and the last MiB.
/// Reads at most 2 MiB, so scanning a network share stays quick.
pub fn fingerprint(path: &Path) -> io::Result<(String, u64)> {
    let ht = read_head_tail(path)?;
    Ok((ht.fingerprint(), ht.size))
}

/// The start and end of a file, read through one open handle (each open costs several
/// round trips on a network share).
pub struct HeadTail {
    pub size: u64,
    /// The first MiB (or the whole file if smaller).
    pub head: Vec<u8>,
    /// The bytes the fingerprint uses after `head`: the last MiB for big files, the rest
    /// for files between 1 and 2 MiB, nothing for small ones.
    pub tail: Vec<u8>,
    file: File,
}

pub fn read_head_tail(path: &Path) -> io::Result<HeadTail> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut head = vec![0u8; CHUNK.min(size) as usize];
    let n = read_up_to(&mut file, &mut head)?;
    head.truncate(n);
    let mut tail = Vec::new();
    if size > 2 * CHUNK {
        file.seek(SeekFrom::Start(size - CHUNK))?;
        tail.resize(CHUNK as usize, 0);
        let n = read_up_to(&mut file, &mut tail)?;
        tail.truncate(n);
    } else if size > CHUNK {
        file.read_to_end(&mut tail)?;
    }
    Ok(HeadTail { size, head, tail, file })
}

impl HeadTail {
    pub fn fingerprint(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(b"zdfp1");
        h.update(&self.size.to_le_bytes());
        h.update(&self.head);
        h.update(&self.tail);
        h.finalize().to_hex().to_string()
    }

    /// The whole file, reusing what's already been read: one more sequential read at most.
    pub fn into_full(mut self) -> io::Result<Vec<u8>> {
        let head_len = self.head.len() as u64;
        if head_len >= self.size {
            return Ok(self.head);
        }
        if self.size <= 2 * CHUNK {
            let mut all = self.head;
            all.extend_from_slice(&self.tail);
            return Ok(all);
        }
        let mut all = self.head;
        all.reserve((self.size - head_len) as usize);
        self.file.seek(SeekFrom::Start(head_len))?;
        self.file.read_to_end(&mut all)?;
        Ok(all)
    }
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
    fn head_tail_reassembles_the_whole_file() {
        let d = tempfile::tempdir().unwrap();
        for len in [0usize, 10, 1_500_000, 3_000_000] {
            let p = d.path().join(format!("{len}.bin"));
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            std::fs::write(&p, &data).unwrap();
            let ht = read_head_tail(&p).unwrap();
            assert_eq!(ht.fingerprint(), fingerprint(&p).unwrap().0);
            assert_eq!(ht.into_full().unwrap(), data, "len {len}");
        }
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
