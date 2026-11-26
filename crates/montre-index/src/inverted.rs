use indexmap::IndexMap;
use montre_core::Position;
use rayon::prelude::*;
use roaring::{MultiOps, RoaringBitmap};
use rustc_hash::{FxBuildHasher, FxHashMap};
use serde::{Deserialize, Serialize};

use crate::{IndexError, Result};

pub trait InvertedIndex {
	fn get(&self, layer: &str, value: &str) -> Option<&RoaringBitmap>;
	fn layers(&self) -> Vec<&str>;
	fn values(&self, layer: &str) -> Option<Vec<&str>>;

	fn value_count(&self, layer: &str) -> Option<usize> {
		self.values(layer).map(|v| v.len())
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InMemoryInverted {
	data: FxHashMap<String, Values>,
}

// indexed, so that a scan over every value can be split across threads
type Values = IndexMap<String, RoaringBitmap, FxBuildHasher>;

impl InMemoryInverted {
	pub fn new() -> Self {
		Self {
			data: FxHashMap::default(),
		}
	}

	pub fn insert(
		&mut self,
		layer: &str,
		value: &str,
		positions: impl IntoIterator<Item = Position>,
	) {
		let bitmap = self
			.data
			.entry(layer.to_string())
			.or_default()
			.entry(value.to_string())
			.or_default();
		for pos in positions {
			bitmap.insert(pos as u32);
		}
	}
}

// the bincode layout of `InMemoryInverted`, borrowed from the file's bytes;
// a map and a sequence of pairs are encoded alike
#[derive(Deserialize)]
struct Stored<'a> {
	#[serde(borrow)]
	data: Vec<(&'a str, StoredValues<'a>)>,
}

// each value with the serialized bytes of its bitmap
type StoredValues<'a> = Vec<(&'a str, &'a [u8])>;

impl InMemoryInverted {
	/// Read the bincode encoding of an index, parsing its bitmaps in parallel.
	pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
		let stored: Stored = bincode::deserialize(bytes)
			.map_err(|error| IndexError::Format(format!("inverted index: {error}")))?;
		let data = stored
			.data
			.into_iter()
			.map(|(layer, values)| {
				let values = values
					.into_par_iter()
					.map(|(value, bitmap)| {
						Ok((value.to_owned(), RoaringBitmap::deserialize_from(bitmap)?))
					})
					.collect::<Result<Values>>()?;
				Ok((layer.to_owned(), values))
			})
			.collect::<Result<_>>()?;
		Ok(Self { data })
	}

	/// The positions of every value of `layer` for which `matches` holds.
	/// Each thread works with its own clone of `matches`, so that a matcher
	/// holding a cache, as a regex does, need not share it.
	pub fn positions_where(
		&self,
		layer: &str,
		matches: impl Fn(&str) -> bool + Clone + Send + Sync,
	) -> RoaringBitmap {
		let Some(values) = self.data.get(layer) else {
			return RoaringBitmap::new();
		};
		let matching: Vec<&RoaringBitmap> = values
			.par_iter()
			.with_min_len(4096)
			.map_init(
				|| matches.clone(),
				|matches, (value, bitmap)| matches(value).then_some(bitmap),
			)
			.flatten_iter()
			.collect();
		matching.union()
	}

	pub fn merge_from(&mut self, other: Self, position_offset: u32) {
		for (layer, values) in other.data {
			let target_layer = self.data.entry(layer).or_default();
			for (value, bitmap) in values {
				let shifted: RoaringBitmap = bitmap.iter().map(|p| p + position_offset).collect();
				target_layer
					.entry(value)
					.and_modify(|existing| *existing |= &shifted)
					.or_insert(shifted);
			}
		}
	}
}

impl Default for InMemoryInverted {
	fn default() -> Self {
		Self::new()
	}
}

impl InvertedIndex for InMemoryInverted {
	fn get(&self, layer: &str, value: &str) -> Option<&RoaringBitmap> {
		self.data.get(layer)?.get(value)
	}

	fn layers(&self) -> Vec<&str> {
		self.data.keys().map(|s| s.as_str()).collect()
	}

	fn values(&self, layer: &str) -> Option<Vec<&str>> {
		self.data
			.get(layer)
			.map(|values| values.keys().map(|s| s.as_str()).collect())
	}

	fn value_count(&self, layer: &str) -> Option<usize> {
		self.data.get(layer).map(|values| values.len())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn basic_insert_and_get() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5, 10]);
		index.insert("word", "cat", [3, 8]);

		let the_positions = index.get("word", "the").unwrap();
		assert!(the_positions.contains(0));
		assert!(the_positions.contains(5));
		assert!(the_positions.contains(10));
		assert!(!the_positions.contains(3));

		let cat_positions = index.get("word", "cat").unwrap();
		assert!(cat_positions.contains(3));
		assert!(cat_positions.contains(8));
	}

	#[test]
	fn get_missing_layer() {
		let index = InMemoryInverted::new();
		assert!(index.get("word", "the").is_none());
	}

	#[test]
	fn get_missing_value() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		assert!(index.get("word", "elephant").is_none());
	}

	#[test]
	fn layers_returns_all() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		index.insert("pos", "DET", [0]);
		index.insert("lemma", "the", [0]);

		let mut layers = index.layers();
		layers.sort();
		assert_eq!(layers, vec!["lemma", "pos", "word"]);
	}

	#[test]
	fn values_for_layer() {
		let mut index = InMemoryInverted::new();
		index.insert("pos", "DET", [0, 5]);
		index.insert("pos", "NOUN", [3, 8]);
		index.insert("pos", "VERB", [4]);

		let mut values = index.values("pos").unwrap();
		values.sort();
		assert_eq!(values, vec!["DET", "NOUN", "VERB"]);
	}

	#[test]
	fn values_missing_layer() {
		let index = InMemoryInverted::new();
		assert!(index.values("nonexistent").is_none());
	}

	#[test]
	fn value_count_matches_values_len() {
		let mut index = InMemoryInverted::new();
		index.insert("pos", "DET", [0, 5]);
		index.insert("pos", "NOUN", [3, 8]);
		index.insert("pos", "VERB", [4]);
		index.insert("word", "the", [0]);

		assert_eq!(index.value_count("pos"), Some(3));
		assert_eq!(index.value_count("word"), Some(1));
		assert_eq!(index.value_count("nonexistent"), None);
	}

	#[test]
	fn incremental_insert() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		index.insert("word", "the", [5, 10]);

		let positions = index.get("word", "the").unwrap();
		assert_eq!(positions.len(), 3);
	}

	#[test]
	fn from_bytes_reads_what_bincode_writes() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5, 10]);
		index.insert("word", "cat", [3, 8]);
		index.insert("pos", "DET", [0, 5]);

		let read = InMemoryInverted::from_bytes(&bincode::serialize(&index).unwrap()).unwrap();
		assert_eq!(read.get("word", "the"), index.get("word", "the"));
		assert_eq!(read.get("word", "cat"), index.get("word", "cat"));
		assert_eq!(read.get("pos", "DET"), index.get("pos", "DET"));
		assert_eq!(read.value_count("word"), Some(2));
	}

	#[test]
	fn from_bytes_reads_indexes_written_with_std_maps() {
		let mut layer = std::collections::HashMap::new();
		layer.insert("the".to_string(), RoaringBitmap::from_iter([0, 5]));
		layer.insert("cat".to_string(), RoaringBitmap::from_iter([3]));
		let mut data = std::collections::HashMap::new();
		data.insert("word".to_string(), layer);

		let read = InMemoryInverted::from_bytes(&bincode::serialize(&data).unwrap()).unwrap();
		assert_eq!(read.get("word", "the").unwrap().len(), 2);
		assert_eq!(read.get("word", "cat").unwrap().len(), 1);
	}

	#[test]
	fn from_bytes_rejects_truncated_input() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5, 10]);
		let bytes = bincode::serialize(&index).unwrap();
		assert!(InMemoryInverted::from_bytes(&bytes[..bytes.len() - 3]).is_err());
	}

	fn small_index_bytes() -> Vec<u8> {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5, 10, 70_000]);
		index.insert("word", "cat", 3..5000);
		index.insert("pos", "DET", [0, 5]);
		bincode::serialize(&index).unwrap()
	}

	#[test]
	fn from_bytes_rejects_every_truncation() {
		let bytes = small_index_bytes();
		for length in 0..bytes.len() {
			assert!(
				InMemoryInverted::from_bytes(&bytes[..length]).is_err(),
				"length {length}"
			);
		}
	}

	#[test]
	fn from_bytes_fails_cleanly_on_any_corrupted_byte() {
		let bytes = small_index_bytes();
		for offset in 0..bytes.len() {
			for flip in [0x01, 0x80, 0xff] {
				let mut corrupted = bytes.clone();
				corrupted[offset] ^= flip;
				let _ = InMemoryInverted::from_bytes(&corrupted);
			}
		}
	}

	#[test]
	fn from_bytes_rejects_lengths_past_the_input_without_allocating_them() {
		let mut huge_layer_count = u64::MAX.to_le_bytes().to_vec();
		huge_layer_count.extend_from_slice(&[0; 64]);
		assert!(InMemoryInverted::from_bytes(&huge_layer_count).is_err());

		// one layer "w" holding one value "v" whose bitmap claims u32::MAX containers
		let mut huge_bitmap = Vec::new();
		huge_bitmap.extend_from_slice(&1u64.to_le_bytes());
		huge_bitmap.extend_from_slice(&1u64.to_le_bytes());
		huge_bitmap.push(b'w');
		huge_bitmap.extend_from_slice(&1u64.to_le_bytes());
		huge_bitmap.extend_from_slice(&1u64.to_le_bytes());
		huge_bitmap.push(b'v');
		let bitmap = [12346u32.to_le_bytes(), u32::MAX.to_le_bytes()].concat();
		huge_bitmap.extend_from_slice(&(bitmap.len() as u64).to_le_bytes());
		huge_bitmap.extend_from_slice(&bitmap);
		// framing intact, so the refusal is the bitmap's own
		assert!(matches!(
			InMemoryInverted::from_bytes(&huge_bitmap),
			Err(IndexError::Io(_))
		));
	}

	#[test]
	fn positions_where_unions_matching_values() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5]);
		index.insert("word", "then", [7]);
		index.insert("word", "cat", [3]);

		let positions = index.positions_where("word", |value| value.starts_with("the"));
		assert_eq!(positions.iter().collect::<Vec<_>>(), vec![0, 5, 7]);
		assert!(index.positions_where("word", |_| false).is_empty());
		assert!(index.positions_where("nonexistent", |_| true).is_empty());
	}
}
