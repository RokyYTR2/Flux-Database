use criterion::{Criterion, criterion_group, criterion_main};
use fluxdb::engine::Engine;
use fluxdb::security::{AuditLogger, CryptoManager, Identity, Role};
use tempfile::TempDir;

fn setup_engine() -> (Engine, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let key = CryptoManager::generate_base64_key();
    let crypto = CryptoManager::from_base64_key(&key).expect("key parse");
    let audit = AuditLogger::open(tmp.path()).expect("audit open");
    let identity = Identity {
        username: "bench_admin".to_string(),
        role: Role::Admin,
    };
    let engine = Engine::open(tmp.path(), crypto, identity, audit).expect("engine open");
    (engine, tmp)
}

fn setup_with_table() -> (Engine, TempDir) {
    let (mut engine, tmp) = setup_engine();
    engine
        .execute_script("CREATE TABLE bench (id INT, name TEXT, active BOOL);")
        .expect("create table");
    (engine, tmp)
}

fn setup_with_rows(n: usize) -> (Engine, TempDir) {
    let (mut engine, tmp) = setup_with_table();
    for i in 0..n {
        engine
            .execute_script(&format!(
                "INSERT INTO bench VALUES ({i}, 'user_{i}', {});",
                i % 2 == 0
            ))
            .expect("insert");
    }
    (engine, tmp)
}

fn bench_create_table(c: &mut Criterion) {
    c.bench_function("create_table", |b| {
        b.iter_with_setup(setup_engine, |(mut engine, _tmp)| {
            engine
                .execute_script("CREATE TABLE t (id INT, name TEXT, active BOOL);")
                .expect("create table");
        });
    });
}

fn bench_insert_single(c: &mut Criterion) {
    c.bench_function("insert_single_row", |b| {
        b.iter_with_setup(setup_with_table, |(mut engine, _tmp)| {
            engine
                .execute_script("INSERT INTO bench VALUES (1, 'Alice', true);")
                .expect("insert");
        });
    });
}

fn bench_insert_100(c: &mut Criterion) {
    c.bench_function("insert_100_rows", |b| {
        b.iter_with_setup(setup_with_table, |(mut engine, _tmp)| {
            for i in 0..100 {
                engine
                    .execute_script(&format!(
                        "INSERT INTO bench VALUES ({i}, 'user_{i}', true);"
                    ))
                    .expect("insert");
            }
        });
    });
}

fn bench_select_all_100(c: &mut Criterion) {
    c.bench_function("select_all_from_100_rows", |b| {
        let (engine, _tmp) = setup_with_rows(100);
        let mut engine = engine;
        b.iter(|| {
            engine
                .execute_script("SELECT * FROM bench;")
                .expect("select");
        });
    });
}

fn bench_select_where_100(c: &mut Criterion) {
    c.bench_function("select_where_from_100_rows", |b| {
        let (mut engine, _tmp) = setup_with_rows(100);
        b.iter(|| {
            engine
                .execute_script("SELECT id, name FROM bench WHERE id = 50;")
                .expect("select");
        });
    });
}

fn bench_select_where_indexed_100(c: &mut Criterion) {
    c.bench_function("select_where_indexed_from_100_rows", |b| {
        let (mut engine, _tmp) = setup_with_rows(100);
        engine
            .execute_script("CREATE INDEX idx_bench_id ON bench(id);")
            .expect("create index");
        b.iter(|| {
            engine
                .execute_script("SELECT id, name FROM bench WHERE id = 50;")
                .expect("select");
        });
    });
}

fn bench_select_like_100(c: &mut Criterion) {
    c.bench_function("select_like_from_100_rows", |b| {
        let (mut engine, _tmp) = setup_with_rows(100);
        b.iter(|| {
            engine
                .execute_script("SELECT * FROM bench WHERE name LIKE 'user_5%';")
                .expect("select");
        });
    });
}

fn bench_update_single_100(c: &mut Criterion) {
    c.bench_function("update_single_in_100_rows", |b| {
        let (mut engine, _tmp) = setup_with_rows(100);
        b.iter(|| {
            engine
                .execute_script("UPDATE bench SET active = false WHERE id = 42;")
                .expect("update");
        });
    });
}

fn bench_delete_and_recount(c: &mut Criterion) {
    c.bench_function("delete_single_from_100_rows", |b| {
        b.iter_with_setup(
            || setup_with_rows(100),
            |(mut engine, _tmp)| {
                engine
                    .execute_script("DELETE FROM bench WHERE id = 50;")
                    .expect("delete");
            },
        );
    });
}

fn bench_select_all_1000(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_table");
    group.sample_size(20);

    let (mut engine, _tmp) = setup_with_rows(1000);
    group.bench_function("select_all_from_1000_rows", |b| {
        b.iter(|| {
            engine
                .execute_script("SELECT * FROM bench;")
                .expect("select");
        });
    });
    group.finish();
}

fn bench_select_where_1000(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_table_where");
    group.sample_size(20);

    let (mut engine, _tmp) = setup_with_rows(1000);
    group.bench_function("select_where_from_1000_rows_no_index", |b| {
        b.iter(|| {
            engine
                .execute_script("SELECT id FROM bench WHERE id = 500;")
                .expect("select");
        });
    });

    engine
        .execute_script("CREATE INDEX idx_bench_id ON bench(id);")
        .expect("create index");
    group.bench_function("select_where_from_1000_rows_indexed", |b| {
        b.iter(|| {
            engine
                .execute_script("SELECT id FROM bench WHERE id = 500;")
                .expect("select");
        });
    });
    group.finish();
}

fn bench_parser(c: &mut Criterion) {
    c.bench_function("parse_complex_query", |b| {
        b.iter(|| {
            fluxdb::parser::parse_script(
                "SELECT id, name FROM users WHERE id >= 10 AND name LIKE 'A%' OR active = true;",
            )
            .expect("parse");
        });
    });
}

fn bench_crypto_roundtrip(c: &mut Criterion) {
    let key = CryptoManager::generate_base64_key();
    let crypto = CryptoManager::from_base64_key(&key).expect("key");
    let payload = b"benchmark payload data for encryption test";

    c.bench_function("crypto_encrypt_decrypt", |b| {
        b.iter(|| {
            let encrypted = crypto.encrypt_to_base64(payload).expect("encrypt");
            crypto.decrypt_from_base64(&encrypted).expect("decrypt");
        });
    });
}

fn bench_alter_table_add_column(c: &mut Criterion) {
    c.bench_function("alter_table_add_column_100_rows", |b| {
        b.iter_with_setup(
            || setup_with_rows(100),
            |(mut engine, _tmp)| {
                engine
                    .execute_script(
                        "ALTER TABLE bench ADD COLUMN email TEXT DEFAULT 'none';",
                    )
                    .expect("alter table");
            },
        );
    });
}

criterion_group!(
    benches,
    bench_create_table,
    bench_insert_single,
    bench_insert_100,
    bench_select_all_100,
    bench_select_where_100,
    bench_select_where_indexed_100,
    bench_select_like_100,
    bench_update_single_100,
    bench_delete_and_recount,
    bench_select_all_1000,
    bench_select_where_1000,
    bench_parser,
    bench_crypto_roundtrip,
    bench_alter_table_add_column,
);
criterion_main!(benches);
