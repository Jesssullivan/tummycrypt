use tcfs_crypto::{decrypt_chunk, encrypt_chunk, generate_file_key};

fn make_data(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| (i.wrapping_mul(7) ^ (i >> 3)) as u8)
        .collect()
}

#[divan::bench(args = [1024, 65536, 1048576])]
fn bench_encrypt_chunk(bencher: divan::Bencher, size: usize) {
    let file_key = generate_file_key();
    let file_id = [0xABu8; 32];
    let data = make_data(size);
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| {
            encrypt_chunk(
                divan::black_box(&file_key),
                0,
                divan::black_box(&file_id),
                divan::black_box(&data),
            )
            .unwrap()
        });
}

#[divan::bench(args = [1024, 65536, 1048576])]
fn bench_decrypt_chunk(bencher: divan::Bencher, size: usize) {
    let file_key = generate_file_key();
    let file_id = [0xABu8; 32];
    let data = make_data(size);
    let encrypted = encrypt_chunk(&file_key, 0, &file_id, &data).unwrap();
    bencher
        .counter(divan::counter::BytesCount::new(size))
        .bench(|| {
            decrypt_chunk(
                divan::black_box(&file_key),
                0,
                divan::black_box(&file_id),
                divan::black_box(&encrypted),
            )
            .unwrap()
        });
}

fn main() {
    divan::main();
}
