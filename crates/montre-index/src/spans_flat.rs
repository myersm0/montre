use std::fs::File;
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;
use montre_core::{Position, Span};

use crate::spans::{InMemorySpans, SpanIndex};
use crate::{IndexError, Result};

const MAGIC: &[u8; 4] = b"MSPN";
const FORMAT_VERSION: u32 = 1;
const BYTE_ORDER_MARK: u32 = 0x01020304;

const HEADER_SIZE: usize = 16;
const DIR_ENTRY_SIZE: usize = 32;

struct LayerDirectory {
	name: String,
	data_offset: usize,
	span_count: usize,
}

pub struct MappedSpans {
	_mmap: Mmap,
	layers: Vec<LayerDirectory>,
}

impl MappedSpans {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let file = File::open(path.as_ref())?;
		let mmap = unsafe { Mmap::map(&file)? };
		let buf = &mmap[..];

		if buf.len() < HEADER_SIZE {
			return Err(IndexError::Format("spans file too small".into()));
		}

		if &buf[0..4] != MAGIC {
			return Err(IndexError::Format("invalid spans magic".into()));
		}

		let version = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
		if version != FORMAT_VERSION {
			return Err(IndexError::Format(
				format!("spans format version {}, expected {}", version, FORMAT_VERSION),
			));
		}

		let bom = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
		if bom != BYTE_ORDER_MARK {
			return Err(IndexError::Format("endianness mismatch".into()));
		}

		let layer_count = u32::from_ne_bytes(buf[12..16].try_into().unwrap()) as usize;
		let dir_start = HEADER_SIZE;
		let dir_end = dir_start + layer_count * DIR_ENTRY_SIZE;

		if buf.len() < dir_end {
			return Err(IndexError::Format("truncated layer directory".into()));
		}

		let string_pool_start = dir_end;
		let mut layers = Vec::with_capacity(layer_count);

		for i in 0..layer_count {
			let entry = &buf[dir_start + i * DIR_ENTRY_SIZE..];
			let name_offset = u64::from_ne_bytes(entry[0..8].try_into().unwrap()) as usize;
			let name_len = u32::from_ne_bytes(entry[8..12].try_into().unwrap()) as usize;
			let data_offset = u64::from_ne_bytes(entry[16..24].try_into().unwrap()) as usize;
			let span_count = u64::from_ne_bytes(entry[24..32].try_into().unwrap()) as usize;

			let name_start = string_pool_start + name_offset;
			let name_end = name_start + name_len;
			if name_end > buf.len() {
				return Err(IndexError::Format("truncated string pool".into()));
			}

			let name = std::str::from_utf8(&buf[name_start..name_end])
				.map_err(|_| IndexError::Format("invalid layer name".into()))?
				.to_string();

			let span_bytes = span_count * std::mem::size_of::<Span>();
			if data_offset + span_bytes > buf.len() {
				return Err(IndexError::Format("truncated span data".into()));
			}

			if data_offset % std::mem::align_of::<Span>() != 0 {
				return Err(IndexError::Format("misaligned span data".into()));
			}

			layers.push(LayerDirectory { name, data_offset, span_count });
		}

		Ok(Self { _mmap: mmap, layers })
	}

	fn layer_spans(&self, entry: &LayerDirectory) -> &[Span] {
		unsafe {
			let ptr = self._mmap.as_ptr().add(entry.data_offset) as *const Span;
			std::slice::from_raw_parts(ptr, entry.span_count)
		}
	}
}

impl SpanIndex for MappedSpans {
	fn spans(&self, layer: &str) -> Option<&[Span]> {
		let entry = self.layers.iter().find(|l| l.name == layer)?;
		Some(self.layer_spans(entry))
	}

	fn containing(&self, layer: &str, position: Position) -> Option<&Span> {
		let spans = self.spans(layer)?;
		montre_core::span_containing(spans, position).map(|idx| &spans[idx])
	}

	fn layers(&self) -> Vec<&str> {
		self.layers.iter().map(|l| l.name.as_str()).collect()
	}
}

pub enum SpanStore {
	InMemory(InMemorySpans),
	Mapped(MappedSpans),
}

impl SpanIndex for SpanStore {
	fn spans(&self, layer: &str) -> Option<&[Span]> {
		match self {
			Self::InMemory(s) => s.spans(layer),
			Self::Mapped(s) => s.spans(layer),
		}
	}

	fn containing(&self, layer: &str, position: Position) -> Option<&Span> {
		match self {
			Self::InMemory(s) => s.containing(layer, position),
			Self::Mapped(s) => s.containing(layer, position),
		}
	}

	fn layers(&self) -> Vec<&str> {
		match self {
			Self::InMemory(s) => s.layers(),
			Self::Mapped(s) => s.layers(),
		}
	}
}

pub fn write_flat_spans(spans: &InMemorySpans, path: impl AsRef<Path>) -> std::io::Result<()> {
	let mut layer_names: Vec<&str> = spans.layers();
	layer_names.sort();

	let layer_data: Vec<(&str, &[Span])> = layer_names
		.iter()
		.filter_map(|&name| spans.spans(name).map(|s| (name, s)))
		.collect();

	let mut string_pool = Vec::new();
	let mut name_entries: Vec<(u64, u32)> = Vec::new();
	for &(name, _) in &layer_data {
		let offset = string_pool.len() as u64;
		string_pool.extend_from_slice(name.as_bytes());
		name_entries.push((offset, name.len() as u32));
	}

	let dir_end = HEADER_SIZE + layer_data.len() * DIR_ENTRY_SIZE;
	let string_pool_padded = (string_pool.len() + 15) & !15;
	let span_data_start = dir_end + string_pool_padded;

	let mut data_offsets = Vec::new();
	let mut offset = span_data_start;
	for &(_, s) in &layer_data {
		data_offsets.push(offset);
		offset += s.len() * std::mem::size_of::<Span>();
	}

	let mut file = File::create(path)?;

	file.write_all(MAGIC)?;
	file.write_all(&FORMAT_VERSION.to_ne_bytes())?;
	file.write_all(&BYTE_ORDER_MARK.to_ne_bytes())?;
	file.write_all(&(layer_data.len() as u32).to_ne_bytes())?;

	for (i, &(name_offset, name_len)) in name_entries.iter().enumerate() {
		file.write_all(&name_offset.to_ne_bytes())?;
		file.write_all(&name_len.to_ne_bytes())?;
		file.write_all(&0u32.to_ne_bytes())?;
		file.write_all(&(data_offsets[i] as u64).to_ne_bytes())?;
		file.write_all(&(layer_data[i].1.len() as u64).to_ne_bytes())?;
	}

	file.write_all(&string_pool)?;
	let padding = string_pool_padded - string_pool.len();
	if padding > 0 {
		file.write_all(&vec![0u8; padding])?;
	}

	for &(_, spans) in &layer_data {
		let bytes = unsafe {
			std::slice::from_raw_parts(
				spans.as_ptr() as *const u8,
				spans.len() * std::mem::size_of::<Span>(),
			)
		};
		file.write_all(bytes)?;
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn build_test_spans() -> InMemorySpans {
		let mut spans = InMemorySpans::new();
		spans.add_span("sentence", Span::new(0, 5));
		spans.add_span("sentence", Span::new(5, 12));
		spans.add_span("sentence", Span::new(12, 20));
		spans.add_span("document", Span::new(0, 12));
		spans.add_span("document", Span::new(12, 20));
		spans.finalize();
		spans
	}

	#[test]
	fn roundtrip() {
		let original = build_test_spans();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spans.bin");

		write_flat_spans(&original, &path).unwrap();
		let mapped = MappedSpans::open(&path).unwrap();

		let orig_sent = original.spans("sentence").unwrap();
		let mapped_sent = mapped.spans("sentence").unwrap();
		assert_eq!(orig_sent, mapped_sent);

		let orig_doc = original.spans("document").unwrap();
		let mapped_doc = mapped.spans("document").unwrap();
		assert_eq!(orig_doc, mapped_doc);

		assert!(mapped.spans("nonexistent").is_none());
	}

	#[test]
	fn mapped_containing() {
		let original = build_test_spans();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spans.bin");

		write_flat_spans(&original, &path).unwrap();
		let mapped = MappedSpans::open(&path).unwrap();

		assert_eq!(mapped.containing("sentence", 0), Some(&Span::new(0, 5)));
		assert_eq!(mapped.containing("sentence", 4), Some(&Span::new(0, 5)));
		assert_eq!(mapped.containing("sentence", 5), Some(&Span::new(5, 12)));
		assert_eq!(mapped.containing("sentence", 15), Some(&Span::new(12, 20)));
		assert_eq!(mapped.containing("sentence", 20), None);
	}

	#[test]
	fn mapped_layers() {
		let original = build_test_spans();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spans.bin");

		write_flat_spans(&original, &path).unwrap();
		let mapped = MappedSpans::open(&path).unwrap();

		let mut layers = mapped.layers();
		layers.sort();
		assert_eq!(layers, vec!["document", "sentence"]);
	}

	#[test]
	fn empty_spans() {
		let spans = InMemorySpans::new();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spans.bin");

		write_flat_spans(&spans, &path).unwrap();
		let mapped = MappedSpans::open(&path).unwrap();

		assert!(mapped.layers().is_empty());
		assert!(mapped.spans("sentence").is_none());
	}

	#[test]
	fn span_store_delegates() {
		let original = build_test_spans();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("spans.bin");

		write_flat_spans(&original, &path).unwrap();
		let mapped = MappedSpans::open(&path).unwrap();
		let store = SpanStore::Mapped(mapped);

		assert_eq!(
			store.spans("sentence").unwrap(),
			original.spans("sentence").unwrap(),
		);
		assert_eq!(
			store.containing("sentence", 5),
			Some(&Span::new(5, 12)),
		);
	}

	#[test]
	fn span_repr_c_layout() {
		assert_eq!(std::mem::size_of::<Span>(), 16);
		assert_eq!(std::mem::align_of::<Span>(), 8);

		let span = Span::new(0x0102030405060708, 0x090a0b0c0d0e0f10);
		let bytes: &[u8] = unsafe {
			std::slice::from_raw_parts(
				&span as *const Span as *const u8,
				std::mem::size_of::<Span>(),
			)
		};
		let start = u64::from_ne_bytes(bytes[0..8].try_into().unwrap());
		let end = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
		assert_eq!(start, 0x0102030405060708);
		assert_eq!(end, 0x090a0b0c0d0e0f10);
	}
}

#[cfg(test)]
mod proptests {
	use super::*;
	use proptest::prelude::*;
	use proptest::collection::vec as pvec;

	fn sorted_spans_strategy() -> impl Strategy<Value = Vec<Span>> {
		pvec(1u64..100, 0..20).prop_map(|lengths| {
			let mut spans = Vec::new();
			let mut pos = 0;
			for len in lengths {
				spans.push(Span::new(pos, pos + len));
				pos += len;
			}
			spans
		})
	}

	proptest! {
		#[test]
		fn roundtrip_preserves_spans(spans in sorted_spans_strategy()) {
			let mut index = InMemorySpans::new();
			for span in &spans {
				index.add_span("sentence", *span);
			}
			index.finalize();

			let dir = tempfile::tempdir().unwrap();
			let path = dir.path().join("spans.bin");
			write_flat_spans(&index, &path).unwrap();
			let mapped = MappedSpans::open(&path).unwrap();

			let original = index.spans("sentence").unwrap_or(&[]);
			let recovered = mapped.spans("sentence").unwrap_or(&[]);
			prop_assert_eq!(original, recovered);
		}

		#[test]
		fn containing_agrees_with_in_memory(
			spans in sorted_spans_strategy(),
			probe in 0u64..2000,
		) {
			let mut index = InMemorySpans::new();
			for span in &spans {
				index.add_span("sentence", *span);
			}
			index.finalize();

			let dir = tempfile::tempdir().unwrap();
			let path = dir.path().join("spans.bin");
			write_flat_spans(&index, &path).unwrap();
			let mapped = MappedSpans::open(&path).unwrap();

			prop_assert_eq!(
				index.containing("sentence", probe),
				mapped.containing("sentence", probe),
			);
		}
	}
}
