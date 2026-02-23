use tcfs_chunks::{chunk_data, compress, decompress_all, hash_bytes, ChunkSizes};

fn make_data(size: usize) -> Vec<u8> {
    // Semi-realistic data: repeating pattern with some entropy
    (0..size)
        .map(|i| (i.wrapping_mul(7) ^ (i >> 3)) as u8)
        .collect()
}

#[divan::bench(args = [1024, 65536, 1048576, 10485760])]
fn fastcdc_chunk(bencher: divan::Bencher, size: usize) {
    let data = make_data(size);
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| chunk_data(divan::black_box(&data), ChunkSizes::SMALL));
}

#[divan::bench(args = [1024, 65536, 1048576, 10485760])]
fn blake3_hash(bencher: divan::Bencher, size: usize) {
    let data = make_data(size);
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| hash_bytes(divan::black_box(&data)));
}

#[divan::bench(args = [1024, 65536, 1048576, 10485760])]
fn zstd_compress(bencher: divan::Bencher, size: usize) {
    let data = make_data(size);
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| compress(divan::black_box(&data), 1024 * 1024, 3).unwrap());
}

#[divan::bench(args = [1024, 65536, 1048576, 10485760])]
fn zstd_decompress(bencher: divan::Bencher, size: usize) {
    let data = make_data(size);
    let blob = compress(&data, 1024 * 1024, 3).unwrap();
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| decompress_all(divan::black_box(&blob)).unwrap());
}

#[divan::bench(args = [1024, 65536, 1048576, 10485760])]
fn full_pipeline(bencher: divan::Bencher, size: usize) {
    let data = make_data(size);
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| {
            let chunks = chunk_data(divan::black_box(&data), ChunkSizes::SMALL);
            for chunk in &chunks {
                let start = chunk.offset as usize;
                let end = start + chunk.length;
                let slice = &data[start..end];
                let _hash = hash_bytes(slice);
                let _compressed = compress(slice, 1024 * 1024, 3).unwrap();
            }
        });
}

fn main() {
    divan::main();
}
