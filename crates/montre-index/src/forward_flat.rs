use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use montre_core::{Position, Value};
use roaring::RoaringBitmap;

use crate::forward::{ForwardIndex, InMemoryForward};

const MAGIC: &[u8; 4] = b"MFWD";
const FORMAT_VERSION: u32 = 1;
const BYTE_ORDER_MARK: u32 = 0x01020304;

const HEADER_SIZE: usize = 24;
const DIR_ENTRY_SIZE: usize = 64;

pub const ENCODING_DICT: u8 = 0x01;
pub const ENCODING_DENSE_NUMERIC: u8 = 0x02;

pub fn align_to_8(n: usize) -> usize {
	(n + 7) & !7
}

pub struct LayerBuild {
	pub name: String,
	pub encoding: u8,
	pub id_width: u8,
	pub vocab_count: u32,
	pub data_count: u64,
	pub term_offsets: Vec<u32>,
	pub term_strings: Vec<u8>,
	pub bitmap_bytes: Vec<u8>,
	pub data_bytes: Vec<u8>,
}

pub fn build_dict_encoded_layer(name: &str, data: &[Value]) -> LayerBuild {
	let mut bitmap = RoaringBitmap::new();
	let mut raw_values: Vec<&str> = Vec::new();

	for (pos, value) in data.iter().enumerate() {
		if let Value::Str(s) = value {
			if !s.is_empty() {
				bitmap.insert(pos as u32);
				raw_values.push(s.as_str());
			}
		}
	}

	let mut distinct: Vec<&str> = raw_values.iter().copied().collect::<std::collections::HashSet<_>>().into_iter().collect();
	distinct.sort();

	let term_to_id: HashMap<&str, u32> = distinct.iter().enumerate()
		.map(|(i, &t)| (t, i as u32))
		.collect();

	let mut term_offsets = Vec::with_capacity(distinct.len() + 1);
	let mut term_strings = Vec::new();
	for &term in &distinct {
		term_offsets.push(term_strings.len() as u32);
		term_strings.extend_from_slice(term.as_bytes());
	}
	term_offsets.push(term_strings.len() as u32);

	let vocab_count = distinct.len() as u32;
	let id_width = if vocab_count <= 256 { 1u8 }
		else if vocab_count <= 65_536 { 2u8 }
		else { 4u8 };

	let mut data_bytes = Vec::with_capacity(raw_values.len() * id_width as usize);
	for &val in &raw_values {
		let id = term_to_id[val];
		match id_width {
			1 => data_bytes.push(id as u8),
			2 => data_bytes.extend_from_slice(&(id as u16).to_ne_bytes()),
			4 => data_bytes.extend_from_slice(&id.to_ne_bytes()),
			_ => unreachable!(),
		}
	}

	let mut bitmap_bytes = Vec::new();
	bitmap.serialize_into(&mut bitmap_bytes).expect("bitmap serialization failed");

	LayerBuild {
		name: name.to_string(),
		encoding: ENCODING_DICT,
		id_width,
		vocab_count,
		data_count: raw_values.len() as u64,
		term_offsets,
		term_strings,
		bitmap_bytes,
		data_bytes,
	}
}

pub fn build_dense_numeric_layer(name: &str, data: &[Value], token_count: u64) -> LayerBuild {
	let mut data_bytes = Vec::with_capacity(token_count as usize * 4);
	for pos in 0..token_count as usize {
		let val = data.get(pos).and_then(|v| match v {
			Value::Int(n) => Some(*n as u32),
			_ => None,
		}).unwrap_or(0);
		data_bytes.extend_from_slice(&val.to_ne_bytes());
	}

	LayerBuild {
		name: name.to_string(),
		encoding: ENCODING_DENSE_NUMERIC,
		id_width: 0,
		vocab_count: 0,
		data_count: token_count,
		term_offsets: Vec::new(),
		term_strings: Vec::new(),
		bitmap_bytes: Vec::new(),
		data_bytes,
	}
}

pub fn is_numeric_layer(data: &[Value]) -> bool {
	data.iter().any(|v| matches!(v, Value::Int(_)))
}

pub fn term_table_size(layer: &LayerBuild) -> usize {
	layer.term_offsets.len() * 4 + layer.term_strings.len()
}

pub fn write_flat_forward(
	forward: &InMemoryForward,
	path: impl AsRef<Path>,
) -> std::io::Result<()> {
	let token_count = forward.token_count();

	let mut layers: Vec<LayerBuild> = Vec::new();
	for name in forward.layer_names() {
		let data = match forward.layer_data(name) {
			Some(d) => d,
			None => continue,
		};

		if is_numeric_layer(data) {
			layers.push(build_dense_numeric_layer(name, data, token_count));
		} else {
			layers.push(build_dict_encoded_layer(name, data));
		}
	}

	write_mfwd(&layers, token_count, path)
}

pub fn write_mfwd(
	layers: &[LayerBuild],
	token_count: u64,
	path: impl AsRef<Path>,
) -> std::io::Result<()> {
	let mut string_pool = Vec::new();
	let mut name_entries: Vec<(u64, u32)> = Vec::new();
	for layer in layers {
		let offset = string_pool.len() as u64;
		string_pool.extend_from_slice(layer.name.as_bytes());
		name_entries.push((offset, layer.name.len() as u32));
	}

	let dir_end = HEADER_SIZE + layers.len() * DIR_ENTRY_SIZE;
	let string_pool_end = dir_end + align_to_8(string_pool.len());

	let mut section_offsets: Vec<(usize, usize, usize)> = Vec::new();
	let mut offset = string_pool_end;

	for layer in layers {
		if layer.encoding == ENCODING_DICT {
			let vocab_offset = offset;
			offset += align_to_8(term_table_size(layer));

			let bitmap_offset = offset;
			offset += align_to_8(layer.bitmap_bytes.len());

			let data_offset = offset;
			offset += align_to_8(layer.data_bytes.len());

			section_offsets.push((vocab_offset, bitmap_offset, data_offset));
		} else {
			let data_offset = offset;
			offset += align_to_8(layer.data_bytes.len());

			section_offsets.push((0, 0, data_offset));
		}
	}

	let mut file = File::create(path)?;

	file.write_all(MAGIC)?;
	file.write_all(&FORMAT_VERSION.to_ne_bytes())?;
	file.write_all(&BYTE_ORDER_MARK.to_ne_bytes())?;
	file.write_all(&(layers.len() as u32).to_ne_bytes())?;
	file.write_all(&token_count.to_ne_bytes())?;

	for (i, layer) in layers.iter().enumerate() {
		let (name_offset, name_len) = name_entries[i];
		let (vocab_offset, bitmap_offset, data_offset) = section_offsets[i];

		file.write_all(&name_offset.to_ne_bytes())?;
		file.write_all(&name_len.to_ne_bytes())?;
		file.write_all(&[layer.encoding])?;
		file.write_all(&[layer.id_width])?;
		file.write_all(&[0u8; 2])?;
		file.write_all(&(vocab_offset as u64).to_ne_bytes())?;
		file.write_all(&layer.vocab_count.to_ne_bytes())?;
		file.write_all(&[0u8; 4])?;
		file.write_all(&(bitmap_offset as u64).to_ne_bytes())?;
		file.write_all(&(layer.bitmap_bytes.len() as u64).to_ne_bytes())?;
		file.write_all(&(data_offset as u64).to_ne_bytes())?;
		file.write_all(&layer.data_count.to_ne_bytes())?;
	}

	file.write_all(&string_pool)?;
	write_padding(&mut file, string_pool.len(), align_to_8(string_pool.len()))?;

	for layer in layers {
		if layer.encoding == ENCODING_DICT {
			for &off in &layer.term_offsets {
				file.write_all(&off.to_ne_bytes())?;
			}
			file.write_all(&layer.term_strings)?;
			let tt_size = term_table_size(layer);
			write_padding(&mut file, tt_size, align_to_8(tt_size))?;

			file.write_all(&layer.bitmap_bytes)?;
			write_padding(&mut file, layer.bitmap_bytes.len(), align_to_8(layer.bitmap_bytes.len()))?;
		}

		file.write_all(&layer.data_bytes)?;
		write_padding(&mut file, layer.data_bytes.len(), align_to_8(layer.data_bytes.len()))?;
	}

	Ok(())
}

fn write_padding(file: &mut File, current: usize, aligned: usize) -> std::io::Result<()> {
	let pad = aligned - current;
	if pad > 0 {
		file.write_all(&vec![0u8; pad])?;
	}
	Ok(())
}

pub fn read_header(data: &[u8]) -> Option<(u32, u32, u64)> {
	if data.len() < HEADER_SIZE {
		return None;
	}
	if &data[0..4] != MAGIC {
		return None;
	}
	let version = u32::from_ne_bytes(data[4..8].try_into().ok()?);
	let layer_count = u32::from_ne_bytes(data[12..16].try_into().ok()?);
	let token_count = u64::from_ne_bytes(data[16..24].try_into().ok()?);
	Some((version, layer_count, token_count))
}

pub fn read_dir_entry(data: &[u8], index: usize) -> Option<DirEntry> {
	let start = HEADER_SIZE + index * DIR_ENTRY_SIZE;
	let entry = data.get(start..start + DIR_ENTRY_SIZE)?;

	Some(DirEntry {
		name_offset: u64::from_ne_bytes(entry[0..8].try_into().ok()?),
		name_len: u32::from_ne_bytes(entry[8..12].try_into().ok()?),
		encoding: entry[12],
		id_width: entry[13],
		vocab_offset: u64::from_ne_bytes(entry[16..24].try_into().ok()?),
		vocab_count: u32::from_ne_bytes(entry[24..28].try_into().ok()?),
		bitmap_offset: u64::from_ne_bytes(entry[32..40].try_into().ok()?),
		bitmap_len: u64::from_ne_bytes(entry[40..48].try_into().ok()?),
		data_offset: u64::from_ne_bytes(entry[48..56].try_into().ok()?),
		data_count: u64::from_ne_bytes(entry[56..64].try_into().ok()?),
	})
}

#[derive(Debug)]
pub struct DirEntry {
	pub name_offset: u64,
	pub name_len: u32,
	pub encoding: u8,
	pub id_width: u8,
	pub vocab_offset: u64,
	pub vocab_count: u32,
	pub bitmap_offset: u64,
	pub bitmap_len: u64,
	pub data_offset: u64,
	pub data_count: u64,
}

enum MappedLayer {
	DictEncoded {
		bitmap: RoaringBitmap,
		is_dense: bool,
		id_width: u8,
		vocab_offset: usize,
		vocab_count: u32,
		data_offset: usize,
	},
	DenseNumeric {
		data_offset: usize,
		data_count: u64,
	},
}

pub struct MappedForward {
	mmap: memmap2::Mmap,
	token_count: u64,
	layer_map: HashMap<String, usize>,
	layers: Vec<MappedLayer>,
}

impl MappedForward {
	pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
		let file = File::open(path.as_ref())?;
		let mmap = unsafe { memmap2::Mmap::map(&file)? };
		let buf = &mmap[..];

		if buf.len() < HEADER_SIZE {
			return Err(crate::IndexError::Format("forward file too small".into()));
		}
		if &buf[0..4] != MAGIC {
			return Err(crate::IndexError::Format("invalid forward magic".into()));
		}
		let version = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
		if version != FORMAT_VERSION {
			return Err(crate::IndexError::Format(
				format!("forward format version {}, expected {}", version, FORMAT_VERSION),
			));
		}
		let bom = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
		if bom != BYTE_ORDER_MARK {
			return Err(crate::IndexError::Format("endianness mismatch".into()));
		}
		let layer_count = u32::from_ne_bytes(buf[12..16].try_into().unwrap()) as usize;
		let token_count = u64::from_ne_bytes(buf[16..24].try_into().unwrap());

		let string_pool_start = HEADER_SIZE + layer_count * DIR_ENTRY_SIZE;

		let mut layers = Vec::with_capacity(layer_count);
		let mut layer_map = HashMap::with_capacity(layer_count);

		for i in 0..layer_count {
			let entry = read_dir_entry(buf, i).ok_or_else(|| {
				crate::IndexError::Format(format!("truncated directory entry {}", i))
			})?;

			let name_start = string_pool_start + entry.name_offset as usize;
			let name_end = name_start + entry.name_len as usize;
			if name_end > buf.len() {
				return Err(crate::IndexError::Format("truncated string pool".into()));
			}
			let name = std::str::from_utf8(&buf[name_start..name_end])
				.map_err(|_| crate::IndexError::Format("invalid layer name".into()))?
				.to_string();

			let mapped_layer = match entry.encoding {
				ENCODING_DICT => {
					let bitmap = RoaringBitmap::deserialize_from(
						&buf[entry.bitmap_offset as usize..entry.bitmap_offset as usize + entry.bitmap_len as usize]
					).map_err(|e| crate::IndexError::Format(format!("bitmap deserialize: {}", e)))?;

					let is_dense = bitmap.len() == token_count;

					MappedLayer::DictEncoded {
						bitmap,
						is_dense,
						id_width: entry.id_width,
						vocab_offset: entry.vocab_offset as usize,
						vocab_count: entry.vocab_count,
						data_offset: entry.data_offset as usize,
					}
				}
				ENCODING_DENSE_NUMERIC => {
					MappedLayer::DenseNumeric {
						data_offset: entry.data_offset as usize,
						data_count: entry.data_count,
					}
				}
				other => {
					return Err(crate::IndexError::Format(
						format!("unknown encoding kind: 0x{:02x}", other),
					));
				}
			};

			layer_map.insert(name, i);
			layers.push(mapped_layer);
		}

		Ok(Self { mmap, token_count, layer_map, layers })
	}

	fn term_str(&self, layer: &MappedLayer, term_id: u32) -> Option<&str> {
		let (vocab_offset, vocab_count) = match layer {
			MappedLayer::DictEncoded { vocab_offset, vocab_count, .. } => (*vocab_offset, *vocab_count),
			_ => return None,
		};

		if term_id >= vocab_count {
			return None;
		}

		let buf = &self.mmap[..];
		let offsets_start = vocab_offset;
		let strings_start = offsets_start + (vocab_count as usize + 1) * 4;

		let off_pos = offsets_start + term_id as usize * 4;
		let off_next = off_pos + 4;

		let start = u32::from_ne_bytes(buf[off_pos..off_pos + 4].try_into().unwrap()) as usize;
		let end = u32::from_ne_bytes(buf[off_next..off_next + 4].try_into().unwrap()) as usize;

		std::str::from_utf8(&buf[strings_start + start..strings_start + end]).ok()
	}

	fn read_term_id(&self, layer: &MappedLayer, idx: usize) -> Option<u32> {
		let (id_width, data_offset) = match layer {
			MappedLayer::DictEncoded { id_width, data_offset, .. } => (*id_width, *data_offset),
			_ => return None,
		};

		let buf = &self.mmap[..];
		let pos = data_offset + idx * id_width as usize;

		match id_width {
			1 => buf.get(pos).map(|&b| b as u32),
			2 => {
				let bytes = buf.get(pos..pos + 2)?;
				Some(u16::from_ne_bytes(bytes.try_into().unwrap()) as u32)
			}
			4 => {
				let bytes = buf.get(pos..pos + 4)?;
				Some(u32::from_ne_bytes(bytes.try_into().unwrap()))
			}
			_ => None,
		}
	}
}

impl ForwardIndex for MappedForward {
	fn token_count(&self) -> u64 {
		self.token_count
	}

	fn get_str(&self, position: Position, layer: &str) -> Option<&str> {
		let &layer_idx = self.layer_map.get(layer)?;
		let mapped_layer = &self.layers[layer_idx];

		match mapped_layer {
			MappedLayer::DictEncoded { bitmap, is_dense, .. } => {
				let idx = if *is_dense {
					if position >= self.token_count {
						return None;
					}
					position as usize
				} else {
					if !bitmap.contains(position as u32) {
						return None;
					}
					(bitmap.rank(position as u32) - 1) as usize
				};

				let term_id = self.read_term_id(mapped_layer, idx)?;
				self.term_str(mapped_layer, term_id)
			}
			MappedLayer::DenseNumeric { .. } => None,
		}
	}

	fn get_int(&self, position: Position, layer: &str) -> Option<i64> {
		let &layer_idx = self.layer_map.get(layer)?;
		let mapped_layer = &self.layers[layer_idx];

		match mapped_layer {
			MappedLayer::DenseNumeric { data_offset, data_count } => {
				if position >= *data_count {
					return None;
				}
				let buf = &self.mmap[..];
				let pos = data_offset + position as usize * 4;
				let val = u32::from_ne_bytes(buf[pos..pos + 4].try_into().unwrap());
				Some(val as i64)
			}
			MappedLayer::DictEncoded { .. } => None,
		}
	}
}

pub enum ForwardStore {
	InMemory(InMemoryForward),
	Mapped(MappedForward),
}

impl ForwardIndex for ForwardStore {
	fn token_count(&self) -> u64 {
		match self {
			Self::InMemory(f) => f.token_count(),
			Self::Mapped(f) => f.token_count(),
		}
	}

	fn get_str(&self, position: Position, layer: &str) -> Option<&str> {
		match self {
			Self::InMemory(f) => f.get_str(position, layer),
			Self::Mapped(f) => f.get_str(position, layer),
		}
	}

	fn get_int(&self, position: Position, layer: &str) -> Option<i64> {
		match self {
			Self::InMemory(f) => f.get_int(position, layer),
			Self::Mapped(f) => f.get_int(position, layer),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::forward::{ForwardIndex, InMemoryForward};

	fn build_test_forward() -> InMemoryForward {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let pos = fwd.add_layer("pos");

		fwd.set(word, 0, "the".into());
		fwd.set(word, 1, "cat".into());
		fwd.set(word, 2, "sat".into());

		fwd.set(pos, 0, "DET".into());
		fwd.set(pos, 1, "NOUN".into());
		fwd.set(pos, 2, "VERB".into());

		fwd
	}

	#[test]
	fn write_header_valid() {
		let fwd = build_test_forward();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");

		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (version, layer_count, _) = read_header(&data).unwrap();
		assert_eq!(version, FORMAT_VERSION);
		assert_eq!(layer_count, 2);
	}

	#[test]
	fn write_dir_entries() {
		let fwd = build_test_forward();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");

		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (_, layer_count, _) = read_header(&data).unwrap();

		for i in 0..layer_count as usize {
			let entry = read_dir_entry(&data, i).unwrap();

			assert_eq!(entry.encoding, ENCODING_DICT);
			assert_eq!(entry.id_width, 1);
			assert!(entry.vocab_count == 3);
			assert!(entry.data_count == 3);
			assert!(entry.bitmap_len > 0);
			assert!(entry.data_offset % 8 == 0);
		}
	}

	#[test]
	fn write_layer_names_recoverable() {
		let fwd = build_test_forward();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");

		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (_, layer_count, _) = read_header(&data).unwrap();
		let string_pool_start = HEADER_SIZE + layer_count as usize * DIR_ENTRY_SIZE;

		let mut names: Vec<String> = Vec::new();
		for i in 0..layer_count as usize {
			let entry = read_dir_entry(&data, i).unwrap();
			let start = string_pool_start + entry.name_offset as usize;
			let end = start + entry.name_len as usize;
			let name = std::str::from_utf8(&data[start..end]).unwrap();
			names.push(name.to_string());
		}

		assert!(names.contains(&"word".to_string()));
		assert!(names.contains(&"pos".to_string()));
	}

	#[test]
	fn write_term_table_recoverable() {
		let fwd = build_test_forward();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");

		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();

		let entry = read_dir_entry(&data, 1).unwrap();
		let vocab_start = entry.vocab_offset as usize;
		let offsets_end = vocab_start + (entry.vocab_count as usize + 1) * 4;

		let mut term_offsets = Vec::new();
		for i in 0..=entry.vocab_count as usize {
			let off_start = vocab_start + i * 4;
			let off = u32::from_ne_bytes(data[off_start..off_start + 4].try_into().unwrap());
			term_offsets.push(off);
		}

		let strings_start = offsets_end;
		let mut terms = Vec::new();
		for i in 0..entry.vocab_count as usize {
			let s = term_offsets[i] as usize;
			let e = term_offsets[i + 1] as usize;
			let term = std::str::from_utf8(&data[strings_start + s..strings_start + e]).unwrap();
			terms.push(term.to_string());
		}

		let is_word_layer = {
			let string_pool_start = HEADER_SIZE + 2 * DIR_ENTRY_SIZE;
			let name_start = string_pool_start + entry.name_offset as usize;
			let name = std::str::from_utf8(&data[name_start..name_start + entry.name_len as usize]).unwrap();
			name == "word"
		};

		if is_word_layer {
			assert_eq!(terms, vec!["cat", "sat", "the"]);
		} else {
			assert_eq!(terms, vec!["DET", "NOUN", "VERB"]);
		}
	}

	#[test]
	fn write_packed_ids_recoverable() {
		let mut fwd = InMemoryForward::new();
		let pos = fwd.add_layer("pos");

		fwd.set(pos, 0, "NOUN".into());
		fwd.set(pos, 1, "VERB".into());
		fwd.set(pos, 2, "NOUN".into());
		fwd.set(pos, 3, "ADJ".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let entry = read_dir_entry(&data, 0).unwrap();

		assert_eq!(entry.id_width, 1);
		assert_eq!(entry.data_count, 4);

		let ids: Vec<u8> = (0..entry.data_count as usize)
			.map(|i| data[entry.data_offset as usize + i])
			.collect();

		assert_eq!(ids[0], ids[2]);
		assert_ne!(ids[0], ids[1]);
		assert_ne!(ids[0], ids[3]);
	}

	#[test]
	fn write_id_width_u16() {
		let mut fwd = InMemoryForward::new();
		let layer = fwd.add_layer("big");

		for i in 0..300u32 {
			fwd.set(layer, i as u64, Value::from(format!("term_{}", i)));
		}

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let entry = read_dir_entry(&data, 0).unwrap();

		assert_eq!(entry.id_width, 2);
		assert_eq!(entry.vocab_count, 300);
		assert_eq!(entry.data_count, 300);
	}

	#[test]
	fn write_sparse_layer() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let feats = fwd.add_layer("feats.Number");

		for i in 0..10u64 {
			fwd.set(word, i, Value::from(format!("w{}", i)));
		}
		fwd.set(feats, 2, "Sing".into());
		fwd.set(feats, 5, "Plur".into());
		fwd.set(feats, 7, "Sing".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (_, layer_count, _) = read_header(&data).unwrap();
		assert_eq!(layer_count, 2);

		let string_pool_start = HEADER_SIZE + 2 * DIR_ENTRY_SIZE;
		for i in 0..2 {
			let entry = read_dir_entry(&data, i).unwrap();
			let name_start = string_pool_start + entry.name_offset as usize;
			let name = std::str::from_utf8(&data[name_start..name_start + entry.name_len as usize]).unwrap();

			if name == "feats.Number" {
				assert_eq!(entry.vocab_count, 2);
				assert_eq!(entry.data_count, 3);

				let bitmap = RoaringBitmap::deserialize_from(
					&data[entry.bitmap_offset as usize..entry.bitmap_offset as usize + entry.bitmap_len as usize]
				).unwrap();
				assert_eq!(bitmap.len(), 3);
				assert!(bitmap.contains(2));
				assert!(bitmap.contains(5));
				assert!(bitmap.contains(7));
				assert!(!bitmap.contains(0));
			} else if name == "word" {
				assert_eq!(entry.vocab_count, 10);
				assert_eq!(entry.data_count, 10);
			}
		}
	}

	#[test]
	fn write_empty_forward() {
		let fwd = InMemoryForward::new();
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");

		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (version, layer_count, _) = read_header(&data).unwrap();
		assert_eq!(version, FORMAT_VERSION);
		assert_eq!(layer_count, 0);
	}

	#[test]
	fn write_dense_numeric_layer() {
		let mut fwd = InMemoryForward::new();
		let head = fwd.add_layer("head");

		fwd.set(head, 0, Value::Int(2));
		fwd.set(head, 1, Value::Int(0));
		fwd.set(head, 2, Value::Int(0));

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let entry = read_dir_entry(&data, 0).unwrap();

		assert_eq!(entry.encoding, ENCODING_DENSE_NUMERIC);
		assert_eq!(entry.id_width, 0);
		assert_eq!(entry.vocab_count, 0);
		assert_eq!(entry.data_count, 3);
		assert_eq!(entry.bitmap_len, 0);

		let d = entry.data_offset as usize;
		let v0 = u32::from_ne_bytes(data[d..d + 4].try_into().unwrap());
		let v1 = u32::from_ne_bytes(data[d + 4..d + 8].try_into().unwrap());
		let v2 = u32::from_ne_bytes(data[d + 8..d + 12].try_into().unwrap());
		assert_eq!(v0, 2);
		assert_eq!(v1, 0);
		assert_eq!(v2, 0);
	}

	#[test]
	fn all_offsets_aligned() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let pos = fwd.add_layer("pos");
		let feats = fwd.add_layer("feats.Number");

		for i in 0..100u64 {
			fwd.set(word, i, Value::from(format!("word_{}", i)));
			fwd.set(pos, i, "NOUN".into());
		}
		for i in (0..100u64).step_by(3) {
			fwd.set(feats, i, "Sing".into());
		}

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let data = std::fs::read(&path).unwrap();
		let (_, layer_count, _) = read_header(&data).unwrap();

		for i in 0..layer_count as usize {
			let entry = read_dir_entry(&data, i).unwrap();
			if entry.encoding == ENCODING_DICT {
				assert_eq!(entry.vocab_offset as usize % 8, 0, "vocab_offset misaligned for layer {}", i);
				assert_eq!(entry.bitmap_offset as usize % 8, 0, "bitmap_offset misaligned for layer {}", i);
			}
			assert_eq!(entry.data_offset as usize % 8, 0, "data_offset misaligned for layer {}", i);
		}
	}

	#[test]
	fn roundtrip_get_str() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let pos = fwd.add_layer("pos");

		fwd.set(word, 0, "the".into());
		fwd.set(word, 1, "cat".into());
		fwd.set(word, 2, "sat".into());
		fwd.set(pos, 0, "DET".into());
		fwd.set(pos, 1, "NOUN".into());
		fwd.set(pos, 2, "VERB".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let mapped = MappedForward::open(&path).unwrap();

		assert_eq!(mapped.get_str(0, "word"), Some("the"));
		assert_eq!(mapped.get_str(1, "word"), Some("cat"));
		assert_eq!(mapped.get_str(2, "word"), Some("sat"));
		assert_eq!(mapped.get_str(0, "pos"), Some("DET"));
		assert_eq!(mapped.get_str(1, "pos"), Some("NOUN"));
		assert_eq!(mapped.get_str(2, "pos"), Some("VERB"));
		assert_eq!(mapped.get_str(3, "word"), None);
		assert_eq!(mapped.get_str(0, "nonexistent"), None);
		assert_eq!(mapped.token_count(), 3);
	}

	#[test]
	fn roundtrip_sparse_layer() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let feats = fwd.add_layer("feats.Number");

		for i in 0..10u64 {
			fwd.set(word, i, Value::from(format!("w{}", i)));
		}
		fwd.set(feats, 2, "Sing".into());
		fwd.set(feats, 5, "Plur".into());
		fwd.set(feats, 7, "Sing".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let mapped = MappedForward::open(&path).unwrap();

		assert_eq!(mapped.get_str(2, "feats.Number"), Some("Sing"));
		assert_eq!(mapped.get_str(5, "feats.Number"), Some("Plur"));
		assert_eq!(mapped.get_str(7, "feats.Number"), Some("Sing"));
		assert_eq!(mapped.get_str(0, "feats.Number"), None);
		assert_eq!(mapped.get_str(1, "feats.Number"), None);
		assert_eq!(mapped.get_str(3, "feats.Number"), None);

		assert_eq!(mapped.get_str(0, "word"), Some("w0"));
		assert_eq!(mapped.get_str(9, "word"), Some("w9"));
	}

	#[test]
	fn roundtrip_dense_numeric() {
		let mut fwd = InMemoryForward::new();
		let head = fwd.add_layer("head");

		fwd.set(head, 0, Value::Int(2));
		fwd.set(head, 1, Value::Int(0));
		fwd.set(head, 2, Value::Int(5));

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let mapped = MappedForward::open(&path).unwrap();

		assert_eq!(mapped.get_int(0, "head"), Some(2));
		assert_eq!(mapped.get_int(1, "head"), Some(0));
		assert_eq!(mapped.get_int(2, "head"), Some(5));
		assert_eq!(mapped.get_int(3, "head"), None);
		assert_eq!(mapped.get_str(0, "head"), None);
	}

	#[test]
	fn roundtrip_mixed_layers() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let pos = fwd.add_layer("pos");
		let head = fwd.add_layer("head");
		let feats = fwd.add_layer("feats.Gender");

		fwd.set(word, 0, "le".into());
		fwd.set(word, 1, "chat".into());
		fwd.set(word, 2, "dort".into());
		fwd.set(pos, 0, "DET".into());
		fwd.set(pos, 1, "NOUN".into());
		fwd.set(pos, 2, "VERB".into());
		fwd.set(head, 0, Value::Int(1));
		fwd.set(head, 1, Value::Int(2));
		fwd.set(head, 2, Value::Int(0));
		fwd.set(feats, 1, "Masc".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let mapped = MappedForward::open(&path).unwrap();

		assert_eq!(mapped.get_str(0, "word"), Some("le"));
		assert_eq!(mapped.get_str(1, "pos"), Some("NOUN"));
		assert_eq!(mapped.get_int(0, "head"), Some(1));
		assert_eq!(mapped.get_int(2, "head"), Some(0));
		assert_eq!(mapped.get_str(1, "feats.Gender"), Some("Masc"));
		assert_eq!(mapped.get_str(0, "feats.Gender"), None);
		assert_eq!(mapped.get_str(2, "feats.Gender"), None);
		assert_eq!(mapped.token_count(), 3);
	}

	#[test]
	fn forward_store_delegates() {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		fwd.set(word, 0, "hello".into());
		fwd.set(word, 1, "world".into());

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("forward.bin");
		write_flat_forward(&fwd, &path).unwrap();

		let mapped = MappedForward::open(&path).unwrap();
		let store = ForwardStore::Mapped(mapped);

		assert_eq!(store.get_str(0, "word"), Some("hello"));
		assert_eq!(store.get_str(1, "word"), Some("world"));
		assert_eq!(store.token_count(), 2);
	}
}
