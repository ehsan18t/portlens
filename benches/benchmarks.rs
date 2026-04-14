//! Benchmarks for `PortLens`.
//!
//! Uses criterion for statistical benchmarking. Setup code (cloning
//! input data) is isolated from measurement via `iter_batched` so
//! results reflect only the code under test.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, criterion_group, criterion_main,
    measurement::WallTime,
};
use portlens::docker;
use portlens::filter::{self, FilterOptions};
use portlens::types::{PortEntry, Protocol, State};

const BENCH_ENTRY_COUNT: u16 = 500;
const PARAMETERIZED_SIZES: &[u16] = &[128, 500, 4096];
const PARAMETERIZED_SAMPLE_SIZE: usize = 50;
const PARAMETERIZED_MEASUREMENT_TIME: Duration = Duration::from_secs(2);
const PARAMETERIZED_WARM_UP_TIME: Duration = Duration::from_secs(1);
const SPARSE_HIT_INTERVAL: u16 = 64;
const ALL_HIT_TOKEN: &str = "needle";
const NO_HIT_TOKEN: &str = "absent";
const EXACT_PROCESS_TOKEN: &str = "target-process";

/// Relative-comparison CI only benchmarks names that already exist on the
/// merge-base so Criterion does not panic on missing baselines.
fn compare_mode() -> bool {
    std::env::var_os("PORTLENS_BENCH_COMPARE").is_some()
}

/// Shared benchmark entry construction.
fn make_entry(i: u16, process: Arc<str>) -> PortEntry {
    PortEntry {
        port: i,
        local_addr: if i.is_multiple_of(2) {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        },
        proto: if i.is_multiple_of(2) {
            Protocol::Tcp
        } else {
            Protocol::Udp
        },
        state: if i.is_multiple_of(2) {
            State::Listen
        } else {
            State::NotApplicable
        },
        pid: 1000 + u32::from(i),
        process,
        user: "user".into(),
        project: if i.is_multiple_of(4) {
            Some(format!("project_{i}"))
        } else {
            None
        },
        app: if i.is_multiple_of(5) {
            Some("Next.js".into())
        } else {
            None
        },
        uptime_secs: Some(u64::from(i) * 3600),
    }
}

/// Build a synthetic dataset of `n` port entries with mixed metadata.
fn synthetic_entries(n: u16) -> Vec<PortEntry> {
    synthetic_entries_with_processes(n, |i| Arc::<str>::from(format!("proc_{i}")))
}

fn synthetic_entries_with_processes(
    n: u16,
    process_name: impl Fn(u16) -> Arc<str>,
) -> Vec<PortEntry> {
    (0..n).map(|i| make_entry(i, process_name(i))).collect()
}

fn exact_process_entries(n: u16) -> Vec<PortEntry> {
    let target_index = n / 2;

    synthetic_entries_with_processes(n, |i| {
        if i == target_index {
            Arc::from(EXACT_PROCESS_TOKEN)
        } else {
            Arc::<str>::from(format!("proc_{i}"))
        }
    })
}

fn grep_all_hit_entries(n: u16) -> Vec<PortEntry> {
    synthetic_entries_with_processes(n, |i| Arc::<str>::from(format!("{ALL_HIT_TOKEN}-proc-{i}")))
}

fn grep_sparse_hit_entries(n: u16) -> Vec<PortEntry> {
    synthetic_entries_with_processes(n, |i| {
        if i.is_multiple_of(SPARSE_HIT_INTERVAL) {
            Arc::<str>::from(format!("{ALL_HIT_TOKEN}-proc-{i}"))
        } else {
            Arc::<str>::from(format!("proc_{i}"))
        }
    })
}

fn grep_no_hit_entries(n: u16) -> Vec<PortEntry> {
    synthetic_entries_with_processes(n, |i| Arc::<str>::from(format!("proc_{i}")))
}

fn bench_filter_case(c: &mut Criterion, name: &str, entries: &[PortEntry], opts: &FilterOptions) {
    c.bench_function(name, |b| {
        b.iter_batched(
            || entries.to_vec(),
            |data| filter::apply(data, opts),
            BatchSize::SmallInput,
        );
    });
}

fn configure_parameterized_group(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.sample_size(PARAMETERIZED_SAMPLE_SIZE);
    group.measurement_time(PARAMETERIZED_MEASUREMENT_TIME);
    group.warm_up_time(PARAMETERIZED_WARM_UP_TIME);
}

fn bench_parameterized_case(
    group: &mut BenchmarkGroup<'_, WallTime>,
    bench_name: &str,
    size: u16,
    entries: &[PortEntry],
    opts: &FilterOptions,
) {
    group.bench_function(BenchmarkId::new(bench_name, size), |b| {
        b.iter_batched(
            || entries.to_vec(),
            |data| filter::apply(data, opts),
            BatchSize::SmallInput,
        );
    });
}

const fn show_all_filter() -> FilterOptions {
    FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        process: None,
        grep: None,
        show_all: true,
    }
}

const fn tcp_only_filter() -> FilterOptions {
    FilterOptions {
        tcp_only: true,
        udp_only: false,
        listen_only: false,
        port: None,
        process: None,
        grep: None,
        show_all: true,
    }
}

const fn relevance_filter() -> FilterOptions {
    FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        process: None,
        grep: None,
        show_all: false,
    }
}

const fn port_filter(size: u16) -> FilterOptions {
    FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: Some(filter::PortFilter::Single(size / 2)),
        process: None,
        grep: None,
        show_all: true,
    }
}

const fn combined_filter() -> FilterOptions {
    FilterOptions {
        tcp_only: true,
        udp_only: false,
        listen_only: true,
        port: None,
        process: None,
        grep: None,
        show_all: false,
    }
}

fn process_filter() -> FilterOptions {
    FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        process: Some(EXACT_PROCESS_TOKEN.to_string()),
        grep: None,
        show_all: true,
    }
}

fn grep_filter(pattern: &str) -> FilterOptions {
    FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        process: None,
        grep: Some(pattern.to_string()),
        show_all: true,
    }
}

fn grep_tcp_filter(pattern: &str) -> FilterOptions {
    FilterOptions {
        tcp_only: true,
        udp_only: false,
        listen_only: false,
        port: None,
        process: None,
        grep: Some(pattern.to_string()),
        show_all: true,
    }
}

fn bench_filter(c: &mut Criterion) {
    let entries = synthetic_entries(BENCH_ENTRY_COUNT);
    let compare_mode = compare_mode();

    bench_filter_case(c, "filter_tcp_only_500", &entries, &tcp_only_filter());
    bench_filter_case(c, "filter_relevance_500", &entries, &relevance_filter());
    bench_filter_case(
        c,
        "filter_port_500",
        &entries,
        &port_filter(BENCH_ENTRY_COUNT),
    );
    bench_filter_case(c, "filter_combined_500", &entries, &combined_filter());

    if !compare_mode {
        bench_filter_case(c, "filter_show_all_500", &entries, &show_all_filter());
    }
}

fn bench_filter_string(c: &mut Criterion) {
    if compare_mode() {
        return;
    }

    let broad_entries = synthetic_entries(BENCH_ENTRY_COUNT);
    let exact_entries = exact_process_entries(BENCH_ENTRY_COUNT);

    bench_filter_case(c, "filter_process_500", &exact_entries, &process_filter());
    bench_filter_case(
        c,
        "filter_grep_broad_500",
        &broad_entries,
        &grep_filter("proc_"),
    );
    bench_filter_case(
        c,
        "filter_grep_narrow_500",
        &broad_entries,
        &grep_filter("proc_25"),
    );
    bench_filter_case(
        c,
        "filter_grep_tcp_500",
        &broad_entries,
        &grep_tcp_filter("proc_25"),
    );
}

fn bench_filter_scale(c: &mut Criterion) {
    if compare_mode() {
        return;
    }

    let mut group = c.benchmark_group("filter_scale");
    configure_parameterized_group(&mut group);

    for &size in PARAMETERIZED_SIZES {
        let entries = synthetic_entries(size);

        bench_parameterized_case(&mut group, "show_all", size, &entries, &show_all_filter());
        bench_parameterized_case(&mut group, "tcp_only", size, &entries, &tcp_only_filter());
        bench_parameterized_case(&mut group, "relevance", size, &entries, &relevance_filter());
        bench_parameterized_case(&mut group, "port_mid", size, &entries, &port_filter(size));
        bench_parameterized_case(&mut group, "combined", size, &entries, &combined_filter());
    }

    group.finish();
}

fn bench_filter_hit_rates(c: &mut Criterion) {
    if compare_mode() {
        return;
    }

    let mut group = c.benchmark_group("filter_hit_rates");
    configure_parameterized_group(&mut group);

    for &size in PARAMETERIZED_SIZES {
        let exact_entries = exact_process_entries(size);
        let all_hit_entries = grep_all_hit_entries(size);
        let sparse_hit_entries = grep_sparse_hit_entries(size);
        let no_hit_entries = grep_no_hit_entries(size);

        bench_parameterized_case(
            &mut group,
            "process_exact",
            size,
            &exact_entries,
            &process_filter(),
        );
        bench_parameterized_case(
            &mut group,
            "grep_all_hits",
            size,
            &all_hit_entries,
            &grep_filter(ALL_HIT_TOKEN),
        );
        bench_parameterized_case(
            &mut group,
            "grep_sparse_hits",
            size,
            &sparse_hit_entries,
            &grep_filter(ALL_HIT_TOKEN),
        );
        bench_parameterized_case(
            &mut group,
            "grep_no_hits",
            size,
            &no_hit_entries,
            &grep_filter(NO_HIT_TOKEN),
        );
        bench_parameterized_case(
            &mut group,
            "grep_sparse_hits_tcp_only",
            size,
            &sparse_hit_entries,
            &grep_tcp_filter(ALL_HIT_TOKEN),
        );
    }

    group.finish();
}

fn bench_docker_parse(c: &mut Criterion) {
    let json = r#"[
        {"Names":["/pg"],"Image":"postgres:16","Ports":[
            {"PrivatePort":5432,"PublicPort":5432,"Type":"tcp"}]},
        {"Names":["/redis"],"Image":"redis:7","Ports":[
            {"PrivatePort":6379,"PublicPort":6379,"Type":"tcp"}]},
        {"Names":["/web"],"Image":"nginx:latest","Ports":[
            {"PrivatePort":80,"PublicPort":8080,"Type":"tcp"},
            {"PrivatePort":443,"PublicPort":8443,"Type":"tcp"}]},
        {"Names":["/api"],"Image":"node","Ports":[
            {"PrivatePort":3000,"PublicPort":3000,"Type":"tcp"}]}
    ]"#;

    c.bench_function("docker_parse_4_containers", |b| {
        b.iter(|| docker::parse_containers_json(json));
    });
}

criterion_group!(
    benches,
    bench_filter,
    bench_filter_string,
    bench_filter_scale,
    bench_filter_hit_rates,
    bench_docker_parse
);
criterion_main!(benches);
