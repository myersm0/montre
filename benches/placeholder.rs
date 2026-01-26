use criterion::{criterion_group, criterion_main, Criterion};

fn placeholder_benchmark(c: &mut Criterion) {
	c.bench_function("placeholder", |b| {
		b.iter(|| {
			let sum: u64 = (0..1000).sum();
			std::hint::black_box(sum)
		})
	});
}

criterion_group!(benches, placeholder_benchmark);
criterion_main!(benches);
