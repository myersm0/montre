use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use roaring::RoaringBitmap;

use crate::{IndexError, Result};

const MAGIC: &[u8; 4] = b"MSPC";
const FORMAT_VERSION: u32 = 1;
const HEADER_SIZE: usize = 8;

pub struct SpacingIndex {
	bitmap: RoaringBitmap,
}

impl SpacingIndex {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let mut file = File::open(path.as_ref())?;
		let mut header = [0u8; HEADER_SIZE];
		file.read_exact(&mut header)?;

		if &header[0..4] != MAGIC {
			return Err(IndexError::Format("invalid spacing magic".into()));
		}
		let version = u32::from_ne_bytes(header[4..8].try_into().unwrap());
		if version != FORMAT_VERSION {
			return Err(IndexError::Format(
				format!("spacing format version {}, expected {}", version, FORMAT_VERSION),
			));
		}

		let bitmap = RoaringBitmap::deserialize_from(&mut file)
			.map_err(|e| IndexError::Format(format!("spacing bitmap: {}", e)))?;

		Ok(Self { bitmap })
	}

	pub fn has_no_space_after(&self, position: u64) -> bool {
		self.bitmap.contains(position as u32)
	}

	pub fn len(&self) -> u64 {
		self.bitmap.len()
	}

	pub fn is_empty(&self) -> bool {
		self.bitmap.is_empty()
	}
}

pub fn write_spacing(bitmap: &RoaringBitmap, path: impl AsRef<Path>) -> std::io::Result<()> {
	let mut file = File::create(path)?;
	file.write_all(MAGIC)?;
	file.write_all(&FORMAT_VERSION.to_ne_bytes())?;
	bitmap.serialize_into(&mut file)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip_spacing() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spacing.bin");

		let mut bitmap = RoaringBitmap::new();
		bitmap.insert(3);
		bitmap.insert(10);
		bitmap.insert(42);

		write_spacing(&bitmap, &path).unwrap();
		let mapped = SpacingIndex::open(&path).unwrap();

		assert!(mapped.has_no_space_after(3));
		assert!(mapped.has_no_space_after(10));
		assert!(mapped.has_no_space_after(42));
		assert!(!mapped.has_no_space_after(0));
		assert!(!mapped.has_no_space_after(4));
		assert_eq!(mapped.len(), 3);
	}

	#[test]
	fn roundtrip_empty() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spacing.bin");

		let bitmap = RoaringBitmap::new();
		write_spacing(&bitmap, &path).unwrap();
		let mapped = SpacingIndex::open(&path).unwrap();

		assert!(mapped.is_empty());
		assert!(!mapped.has_no_space_after(0));
	}
}
