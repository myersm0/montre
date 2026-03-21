use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use roaring::RoaringBitmap;

const TOKEN_COUNT: u32 = 10_000_000;

fn simple_hash(mut x: u64) -> u64 {
	x ^= x >> 33;
	x = x.wrapping_mul(0xff51afd7ed558ccd);
	x ^= x >> 33;
	x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
	x ^= x >> 33;
	x
}

struct DensityCase {
	label: &'static str,
	fraction: f64,
}

fn density_cases() -> Vec<DensityCase> {
	vec![
		DensityCase { label: "100.0%", fraction: 1.0 },
		DensityCase { label: "54.0%", fraction: 0.54 },
		DensityCase { label: "35.0%", fraction: 0.35 },
		DensityCase { label: "10.0%", fraction: 0.10 },
		DensityCase { label: "1.0%", fraction: 0.01 },
		DensityCase { label: "0.001%", fraction: 0.00001 },
	]
}

fn build_bitmap(fraction: f64) -> RoaringBitmap {
	if fraction >= 1.0 {
		let mut bm = RoaringBitmap::new();
		bm.insert_range(0..TOKEN_COUNT);
		return bm;
	}

	let threshold = (fraction * u64::MAX as f64) as u64;
	let mut bm = RoaringBitmap::new();
	for pos in 0..TOKEN_COUNT {
		if simple_hash(pos as u64) < threshold {
			bm.insert(pos);
		}
	}
	bm
}

fn probe_positions() -> Vec<u32> {
	(0..10_000)
		.map(|i| (simple_hash(i as u64 + 0xdeadbeef) % TOKEN_COUNT as u64) as u32)
		.collect()
}

fn bench_contains(c: &mut Criterion) {
	let cases = density_cases();
	let probes = probe_positions();
	let mut group = c.benchmark_group("roaring_contains");
	group.measurement_time(Duration::from_secs(5));

	for case in &cases {
		let bitmap = build_bitmap(case.fraction);
		group.bench_with_input(
			BenchmarkId::new("contains", case.label),
			&(),
			|b, _| {
				b.iter(|| {
					let mut hits = 0u32;
					for &pos in &probes {
						if bitmap.contains(black_box(pos)) {
							hits += 1;
						}
					}
					hits
				})
			},
		);
	}
	group.finish();
}

fn bench_rank(c: &mut Criterion) {
	let cases = density_cases();
	let probes = probe_positions();
	let mut group = c.benchmark_group("roaring_rank");
	group.measurement_time(Duration::from_secs(5));

	for case in &cases {
		let bitmap = build_bitmap(case.fraction);
		group.bench_with_input(
			BenchmarkId::new("rank", case.label),
			&(),
			|b, _| {
				b.iter(|| {
					let mut total = 0u64;
					for &pos in &probes {
						total += bitmap.rank(black_box(pos)) as u64;
					}
					total
				})
			},
		);
	}
	group.finish();
}

fn bench_contains_then_rank(c: &mut Criterion) {
	let cases = density_cases();
	let probes = probe_positions();
	let mut group = c.benchmark_group("roaring_contains_rank");
	group.measurement_time(Duration::from_secs(5));

	for case in &cases {
		let bitmap = build_bitmap(case.fraction);
		group.bench_with_input(
			BenchmarkId::new("contains+rank", case.label),
			&(),
			|b, _| {
				b.iter(|| {
					let mut total = 0u64;
					for &pos in &probes {
						let pos = black_box(pos);
						if bitmap.contains(pos) {
							total += bitmap.rank(pos) as u64;
						}
					}
					total
				})
			},
		);
	}
	group.finish();
}

fn bench_bitmap_stats(c: &mut Criterion) {
	let cases = density_cases();
	let mut group = c.benchmark_group("roaring_stats");
	group.measurement_time(Duration::from_secs(1));

	for case in &cases {
		let bitmap = build_bitmap(case.fraction);
		let serialized = bitmap.serialized_size();
		let cardinality = bitmap.len();

		group.bench_with_input(
			BenchmarkId::new("cardinality_check", case.label),
			&(),
			|b, _| {
				b.iter(|| bitmap.len() == black_box(TOKEN_COUNT as u64))
			},
		);

		eprintln!(
			"  {} density={} cardinality={} serialized_bytes={}",
			case.label, case.fraction, cardinality, serialized
		);
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_bitmap_stats,
	bench_contains,
	bench_rank,
	bench_contains_then_rank,
);
criterion_main!(benches);
