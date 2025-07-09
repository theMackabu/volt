use merkle_hash::{Algorithm, MerkleTree};
use rayon::prelude::*;
use std::{collections::hash_map::DefaultHasher, hash::Hasher, path::Path, time::UNIX_EPOCH};

const SAMPLE_RATE: f32 = 0.1;
const CHUNK_SIZE: usize = 64 * 1024;

const MERKLE_TREE_THRESHOLD: usize = 1000;
const DEFAULT_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn bytes_to_hex(bytes: impl AsRef<[u8]>) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";

    let bytes = bytes.as_ref();
    let len = bytes.len();
    let mut hex_string = String::with_capacity(len * 2);

    for &byte in bytes {
        hex_string.push(TABLE[(byte >> 4) as usize] as char);
        hex_string.push(TABLE[(byte & 0xf) as usize] as char);
    }

    hex_string
}

#[inline]
fn should_sample(path: &Path) -> bool {
    let mut hasher = DefaultHasher::new();
    hasher.write(path.as_os_str().as_encoded_bytes());
    (hasher.finish() as f32 / u64::MAX as f32) < SAMPLE_RATE
}

#[inline]
fn hash_metadata(hasher: &mut DefaultHasher, path: &Path) {
    hasher.write(path.as_os_str().as_encoded_bytes());

    if let Ok(metadata) = std::fs::metadata(path) {
        hasher.write_u64(metadata.len());

        let modified_secs = metadata.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        hasher.write_u64(modified_secs);
    }
}

#[inline]
fn hash_file_sample(hasher: &mut DefaultHasher, path: &Path) {
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buffer = vec![0u8; CHUNK_SIZE];
        if let Ok(bytes_read) = std::io::Read::read(&mut file, &mut buffer) {
            hasher.write(&buffer[..bytes_read]);
        }
    }
}

fn compute_cache_merkle(dir: &str) -> Result<String, std::io::Error> {
    let path = Path::new(dir);
    if !path.exists() {
        return Ok(DEFAULT_HASH.to_string());
    }

    let path_str = match path.to_str() {
        Some(path) => path,
        None => {
            return Ok(DEFAULT_HASH.to_string());
        }
    };

    match MerkleTree::builder(path_str).algorithm(Algorithm::Blake3).hash_names(false).build() {
        Ok(tree) => {
            let hash = bytes_to_hex(tree.root.item.hash);
            Ok(hash)
        }
        Err(_) => compute_cache_sampling(&[dir.to_string()]),
    }
}

fn compute_cache_merkle_multi(dirs: &[String]) -> Result<String, std::io::Error> {
    let mut merkle_hashes = Vec::new();

    for dir in dirs {
        let hash = compute_cache_merkle(dir)?;
        merkle_hashes.push(hash);
    }

    merkle_hashes.sort();
    let mut result = String::with_capacity(64);

    for i in 0..4 {
        let combined = merkle_hashes.iter().fold(0u64, |acc, hash_str| {
            let truncated_hash = if hash_str.len() > 16 { &hash_str[..16] } else { hash_str };
            let hash_val = u64::from_str_radix(truncated_hash, 16).unwrap_or(0);
            acc ^ hash_val.wrapping_add(i)
        });

        for shift in (0..64).step_by(4).rev() {
            let nibble = (combined >> shift) & 0xf;
            result.push(if nibble < 10 { (b'0' + nibble as u8) as char } else { (b'a' + (nibble - 10) as u8) as char });
        }
    }

    Ok(result)
}

fn compute_cache_sampling(dirs: &[String]) -> Result<String, std::io::Error> {
    let mut all_files = Vec::new();

    for dir in dirs {
        let files: Vec<_> = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_owned())
            .collect();
        all_files.extend(files);
    }

    all_files.sort();

    let hashes: Vec<u64> = all_files
        .par_iter()
        .map(|path| {
            let mut hasher = DefaultHasher::new();

            hash_metadata(&mut hasher, path);

            if should_sample(path) {
                hash_file_sample(&mut hasher, path);
            }

            hasher.finish()
        })
        .collect();

    let final_hash = hashes.iter().fold(0u64, |a, b| a ^ b);

    Ok(format!("{:x}", final_hash))
}

fn count_files_in_dir(dir: &str) -> usize { walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).count() }

pub fn compute_cache(dirs: &[String]) -> Result<String, std::io::Error> {
    if dirs.is_empty() {
        return Ok(DEFAULT_HASH.to_string());
    }

    if dirs.len() == 1 {
        return compute_cache_merkle(&dirs[0]);
    }

    let total_files: usize = dirs.iter().map(|d| count_files_in_dir(d)).sum();

    match total_files <= MERKLE_TREE_THRESHOLD {
        true => compute_cache_merkle_multi(dirs),
        false => compute_cache_sampling(dirs),
    }
}
