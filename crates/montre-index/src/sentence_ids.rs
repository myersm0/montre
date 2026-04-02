use std::fs::File;
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;

use crate::{IndexError, Result};

const MAGIC: &[u8; 4] = b"MSID";
const FORMAT_VERSION: u32 = 1;
const BYTE_ORDER_MARK: u32 = 0x01020304;
const HEADER_SIZE: usize = 16;

fn align_to_8(n: usize) -> usize {
	(n + 7) & !7
}

pub struct MappedSentenceIds {
	mmap: Mmap,
	count: u32,
	offsets_start: usize,
	pool_start: usize,
}

impl MappedSentenceIds {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let file = File::open(path.as_ref())?;
		let mmap = unsafe { Mmap::map(&file)? };
		let buf = &mmap[..];

		if buf.len() < HEADER_SIZE {
			return Err(IndexError::Format("sentence_ids file too small".into()));
		}
		if &buf[0..4] != MAGIC {
			return Err(IndexError::Format("invalid sentence_ids magic".into()));
		}
		let version = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
		if version != FORMAT_VERSION {
			return Err(IndexError::Format(
				format!("sentence_ids format version {}, expected {}", version, FORMAT_VERSION),
			));
		}
		let bom = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
		if bom != BYTE_ORDER_MARK {
			return Err(IndexError::Format("sentence_ids endianness mismatch".into()));
		}
		let count = u32::from_ne_bytes(buf[12..16].try_into().unwrap());

		let offsets_start = HEADER_SIZE;
		let offsets_end = offsets_start + (count as usize + 1) * 4;
		let pool_start = align_to_8(offsets_end);

		if pool_start > buf.len() {
			return Err(IndexError::Format("sentence_ids truncated offset table".into()));
		}

		Ok(Self { mmap, count, offsets_start, pool_start })
	}

	pub fn len(&self) -> usize {
		self.count as usize
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	pub fn get(&self, index: usize) -> Option<&str> {
		if index >= self.count as usize {
			return None;
		}
		let buf = &self.mmap[..];
		let off_pos = self.offsets_start + index * 4;
		let start = u32::from_ne_bytes(buf[off_pos..off_pos + 4].try_into().unwrap()) as usize;
		let end = u32::from_ne_bytes(buf[off_pos + 4..off_pos + 8].try_into().unwrap()) as usize;
		std::str::from_utf8(&buf[self.pool_start + start..self.pool_start + end]).ok()
	}
}

pub fn write_sentence_ids(ids: &[String], path: impl AsRef<Path>) -> std::io::Result<()> {
	let count = ids.len() as u32;

	let mut offsets = Vec::with_capacity(ids.len() + 1);
	let mut pool = Vec::new();
	for id in ids {
		offsets.push(pool.len() as u32);
		pool.extend_from_slice(id.as_bytes());
	}
	offsets.push(pool.len() as u32);

	let offsets_end = HEADER_SIZE + offsets.len() * 4;
	let pool_start = align_to_8(offsets_end);
	let pad = pool_start - offsets_end;

	let mut file = File::create(path)?;
	file.write_all(MAGIC)?;
	file.write_all(&FORMAT_VERSION.to_ne_bytes())?;
	file.write_all(&BYTE_ORDER_MARK.to_ne_bytes())?;
	file.write_all(&count.to_ne_bytes())?;

	for &off in &offsets {
		file.write_all(&off.to_ne_bytes())?;
	}
	if pad > 0 {
		file.write_all(&vec![0u8; pad])?;
	}
	file.write_all(&pool)?;

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip_sentence_ids() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("sentence_ids.bin");

		let ids: Vec<String> = vec![
			"doc1:0".into(),
			"doc1:1".into(),
			"test-sent-42".into(),
			"la-parure:127".into(),
		];

		write_sentence_ids(&ids, &path).unwrap();
		let mapped = MappedSentenceIds::open(&path).unwrap();

		assert_eq!(mapped.len(), 4);
		assert_eq!(mapped.get(0), Some("doc1:0"));
		assert_eq!(mapped.get(1), Some("doc1:1"));
		assert_eq!(mapped.get(2), Some("test-sent-42"));
		assert_eq!(mapped.get(3), Some("la-parure:127"));
		assert_eq!(mapped.get(4), None);
	}

	#[test]
	fn roundtrip_empty() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("sentence_ids.bin");

		write_sentence_ids(&[], &path).unwrap();
		let mapped = MappedSentenceIds::open(&path).unwrap();

		assert_eq!(mapped.len(), 0);
		assert!(mapped.is_empty());
		assert_eq!(mapped.get(0), None);
	}

	#[test]
	fn roundtrip_unicode() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("sentence_ids.bin");

		let ids: Vec<String> = vec![
			"données:0".into(),
			"日本語:1".into(),
		];

		write_sentence_ids(&ids, &path).unwrap();
		let mapped = MappedSentenceIds::open(&path).unwrap();

		assert_eq!(mapped.get(0), Some("données:0"));
		assert_eq!(mapped.get(1), Some("日本語:1"));
	}
}

#[cfg(test)]
mod proptests {
	use super::*;
	use proptest::prelude::*;
	use proptest::collection::vec as pvec;

	proptest! {
		#[test]
		fn roundtrip_preserves_ids(ids in pvec("[a-z0-9:_-]{1,40}", 0..50)) {
			let dir = tempfile::tempdir().unwrap();
			let path = dir.path().join("sentence_ids.bin");

			let owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
			write_sentence_ids(&owned, &path).unwrap();
			let mapped = MappedSentenceIds::open(&path).unwrap();

			prop_assert_eq!(mapped.len(), ids.len());
			for (i, id) in ids.iter().enumerate() {
				prop_assert_eq!(mapped.get(i), Some(id.as_str()));
			}
			prop_assert_eq!(mapped.get(ids.len()), None);
		}
	}
}
