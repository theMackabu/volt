use ahash::AHasher;
use rayon::{iter::ParallelBridge, prelude::*};
use std::hash::Hasher;
use std::{path::Path, time::UNIX_EPOCH};

const SAMPLE_RATE: f32 = 0.1;
const CHUNK_SIZE: usize = 4096;

fn should_sample(path: &Path) -> bool {
    let mut hasher = AHasher::default();
    hasher.write(path.to_string_lossy().as_bytes());
    (hasher.finish() as f32 / u64::MAX as f32) < SAMPLE_RATE
}

fn hash_metadata(hasher: &mut AHasher, path: &Path) {
    let path_str = path.to_string_lossy();
    hasher.write(path_str.as_bytes());

    if let Ok(metadata) = std::fs::metadata(path) {
        hasher.write_u64(metadata.len() as u64);

        let modified_secs = metadata.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        hasher.write_u64(modified_secs);
    }
}

pub fn compute_cache(dirs: &[String]) -> Result<String, std::io::Error> {
    let final_hash = dirs
        .par_iter()
        .flat_map(|dir| walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).par_bridge())
        .fold(
            || AHasher::default(),
            |mut hasher, entry| {
                let path = entry.path();
                hash_metadata(&mut hasher, path);

                if should_sample(path) {
                    if let Ok(mut file) = std::fs::File::open(path) {
                        let mut buffer = [0u8; CHUNK_SIZE];
                        let _ = std::io::Read::read(&mut file, &mut buffer);
                        hasher.write(&buffer);
                    }
                }
                hasher
            },
        )
        .reduce(
            || AHasher::default(),
            |mut a, b| {
                a.write_u64(b.finish());
                a
            },
        );

    Ok(format!("{:x}", final_hash.finish()))
}
