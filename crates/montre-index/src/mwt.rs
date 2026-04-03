use std::fs::File;
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;

use crate::{IndexError, Result, align_to_8};

const MAGIC: &[u8; 4] = b"MMWT";
const FORMAT_VERSION: u32 = 1;
const BYTE_ORDER_MARK: u32 = 0x01020304;
const HEADER_SIZE: usize = 16;
const ENTRY_SIZE: usize = 24;

#[derive(Debug, Clone)]
pub struct MWTEntry {
	pub start: u64,
	pub end: u64,
	pub form: String,
	pub no_space_after: bool,
}

pub struct MappedMWTs {
	mmap: Mmap,
	count: u64,
	entries_start: usize,
	pool_start: usize,
}

impl MappedMWTs {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let file = File::open(path.as_ref())?;
		let mmap = unsafe { Mmap::map(&file)? };
		let buf = &mmap[..];

		if buf.len() < HEADER_SIZE {
			return Err(IndexError::Format("mwt file too small".into()));
		}
		if &buf[0..4] != MAGIC {
			return Err(IndexError::Format("invalid mwt magic".into()));
		}
		let version = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
		if version != FORMAT_VERSION {
			return Err(IndexError::Format(
				format!("mwt format version {}, expected {}", version, FORMAT_VERSION),
			));
		}
		let bom = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
		if bom != BYTE_ORDER_MARK {
			return Err(IndexError::Format("mwt endianness mismatch".into()));
		}
		let count = u32::from_ne_bytes(buf[12..16].try_into().unwrap()) as u64;

		let entries_start = HEADER_SIZE;
		let entries_end = entries_start + count as usize * ENTRY_SIZE;
		let pool_start = align_to_8(entries_end);

		if pool_start > buf.len() {
			return Err(IndexError::Format("mwt file truncated".into()));
		}

		Ok(Self { mmap, count, entries_start, pool_start })
	}

	pub fn len(&self) -> usize {
		self.count as usize
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	fn read_entry(&self, index: usize) -> Option<(u64, u64, usize, u32, u8)> {
		if index >= self.count as usize {
			return None;
		}
		let buf = &self.mmap[..];
		let off = self.entries_start + index * ENTRY_SIZE;
		let start = u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap());
		let end = u64::from_ne_bytes(buf[off + 8..off + 16].try_into().unwrap());
		let form_offset = u32::from_ne_bytes(buf[off + 16..off + 20].try_into().unwrap()) as usize;
		let form_len = u16::from_ne_bytes(buf[off + 20..off + 22].try_into().unwrap()) as u32;
		let flags = buf[off + 22];
		Some((start, end, form_offset, form_len, flags))
	}

	pub fn get(&self, index: usize) -> Option<MWTEntry> {
		let (start, end, form_offset, form_len, flags) = self.read_entry(index)?;
		let buf = &self.mmap[..];
		let s = self.pool_start + form_offset;
		let form = std::str::from_utf8(&buf[s..s + form_len as usize]).ok()?.to_string();
		Some(MWTEntry {
			start,
			end,
			form,
			no_space_after: flags & 0x01 != 0,
		})
	}

	pub fn covering(&self, position: u64) -> Option<MWTEntry> {
		let idx = self.search(position)?;
		let entry = self.get(idx)?;
		if position >= entry.start && position < entry.end {
			Some(entry)
		} else {
			None
		}
	}

	pub fn in_range(&self, start: u64, end: u64) -> Vec<MWTEntry> {
		let first = match self.lower_bound(start) {
			Some(i) => i,
			None => return Vec::new(),
		};
		let mut result = Vec::new();
		for i in first..self.count as usize {
			let entry = match self.get(i) {
				Some(e) => e,
				None => break,
			};
			if entry.start >= end {
				break;
			}
			result.push(entry);
		}
		result
	}

	fn search(&self, position: u64) -> Option<usize> {
		let n = self.count as usize;
		if n == 0 {
			return None;
		}
		let mut lo = 0;
		let mut hi = n;
		while lo < hi {
			let mid = lo + (hi - lo) / 2;
			let (start, _, _, _, _) = self.read_entry(mid)?;
			if start <= position {
				lo = mid + 1;
			} else {
				hi = mid;
			}
		}
		if lo > 0 { Some(lo - 1) } else { None }
	}

	fn lower_bound(&self, position: u64) -> Option<usize> {
		let n = self.count as usize;
		if n == 0 {
			return None;
		}
		let mut lo = 0;
		let mut hi = n;
		while lo < hi {
			let mid = lo + (hi - lo) / 2;
			let (_, end, _, _, _) = self.read_entry(mid)?;
			if end <= position {
				lo = mid + 1;
			} else {
				hi = mid;
			}
		}
		if lo < n { Some(lo) } else { None }
	}
}

pub fn write_mwts(entries: &[MWTEntry], path: impl AsRef<Path>) -> std::io::Result<()> {
	let count = entries.len() as u32;

	let mut pool = Vec::new();
	let mut form_entries: Vec<(u32, u16)> = Vec::with_capacity(entries.len());
	for entry in entries {
		let offset = pool.len() as u32;
		let len = entry.form.len() as u16;
		pool.extend_from_slice(entry.form.as_bytes());
		form_entries.push((offset, len));
	}

	let entries_end = HEADER_SIZE + entries.len() * ENTRY_SIZE;
	let pool_start = align_to_8(entries_end);
	let pad = pool_start - entries_end;

	let mut file = File::create(path)?;
	file.write_all(MAGIC)?;
	file.write_all(&FORMAT_VERSION.to_ne_bytes())?;
	file.write_all(&BYTE_ORDER_MARK.to_ne_bytes())?;
	file.write_all(&count.to_ne_bytes())?;

	for (i, entry) in entries.iter().enumerate() {
		let (form_offset, form_len) = form_entries[i];
		let flags: u8 = if entry.no_space_after { 0x01 } else { 0x00 };
		file.write_all(&entry.start.to_ne_bytes())?;
		file.write_all(&entry.end.to_ne_bytes())?;
		file.write_all(&form_offset.to_ne_bytes())?;
		file.write_all(&form_len.to_ne_bytes())?;
		file.write_all(&[flags])?;
		file.write_all(&[0u8])?; // padding to 24 bytes
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
	fn roundtrip_mwts() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("mwt.bin");

		let entries = vec![
			MWTEntry { start: 2, end: 4, form: "au".into(), no_space_after: false },
			MWTEntry { start: 10, end: 12, form: "du".into(), no_space_after: true },
			MWTEntry { start: 20, end: 22, form: "des".into(), no_space_after: false },
		];

		write_mwts(&entries, &path).unwrap();
		let mapped = MappedMWTs::open(&path).unwrap();

		assert_eq!(mapped.len(), 3);

		let e0 = mapped.get(0).unwrap();
		assert_eq!(e0.start, 2);
		assert_eq!(e0.end, 4);
		assert_eq!(e0.form, "au");
		assert!(!e0.no_space_after);

		let e1 = mapped.get(1).unwrap();
		assert_eq!(e1.form, "du");
		assert!(e1.no_space_after);
	}

	#[test]
	fn covering_lookup() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("mwt.bin");

		let entries = vec![
			MWTEntry { start: 5, end: 7, form: "au".into(), no_space_after: false },
			MWTEntry { start: 15, end: 17, form: "du".into(), no_space_after: false },
		];

		write_mwts(&entries, &path).unwrap();
		let mapped = MappedMWTs::open(&path).unwrap();

		assert!(mapped.covering(4).is_none());
		assert_eq!(mapped.covering(5).unwrap().form, "au");
		assert_eq!(mapped.covering(6).unwrap().form, "au");
		assert!(mapped.covering(7).is_none());
		assert_eq!(mapped.covering(15).unwrap().form, "du");
		assert!(mapped.covering(17).is_none());
	}

	#[test]
	fn in_range_lookup() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("mwt.bin");

		let entries = vec![
			MWTEntry { start: 2, end: 4, form: "au".into(), no_space_after: false },
			MWTEntry { start: 10, end: 12, form: "du".into(), no_space_after: false },
			MWTEntry { start: 20, end: 22, form: "des".into(), no_space_after: false },
		];

		write_mwts(&entries, &path).unwrap();
		let mapped = MappedMWTs::open(&path).unwrap();

		let found = mapped.in_range(0, 15);
		assert_eq!(found.len(), 2);
		assert_eq!(found[0].form, "au");
		assert_eq!(found[1].form, "du");

		let found = mapped.in_range(5, 10);
		assert_eq!(found.len(), 0);
	}

	#[test]
	fn roundtrip_empty() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("mwt.bin");

		write_mwts(&[], &path).unwrap();
		let mapped = MappedMWTs::open(&path).unwrap();

		assert_eq!(mapped.len(), 0);
		assert!(mapped.is_empty());
		assert!(mapped.covering(0).is_none());
	}

	#[test]
	fn roundtrip_unicode_form() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("mwt.bin");

		let entries = vec![
			MWTEntry { start: 0, end: 2, form: "l'homme".into(), no_space_after: true },
		];

		write_mwts(&entries, &path).unwrap();
		let mapped = MappedMWTs::open(&path).unwrap();

		let e = mapped.get(0).unwrap();
		assert_eq!(e.form, "l'homme");
		assert!(e.no_space_after);
	}
}
