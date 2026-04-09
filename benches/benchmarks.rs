//! Benchmarks for portview.
//!
//! Uses criterion for statistical benchmarking.

use criterion::{Criterion, criterion_group, criterion_main};

fn bench_filter(c: &mut Criterion) {
    use portview::filter::{self, FilterOptions};
    use portview::types::{PortEntry, Protocol, State};

    let entries: Vec<PortEntry> = (0..500)
        .map(|i| PortEntry {
            port: i,
            proto: if i % 2 == 0 {
                Protocol::Tcp
            } else {
                Protocol::Udp
            },
            state: if i % 3 == 0 {
                State::Listen
            } else {
                State::NotApplicable
            },
            pid: 1000 + u32::from(i),
            process: format!("proc_{i}"),
            user: "user".to_string(),
            project: if i % 4 == 0 {
                Some(format!("project_{i}"))
            } else {
                None
            },
            app: if i % 5 == 0 {
                Some("Next.js".to_string())
            } else {
                None
            },
            uptime_secs: Some(u64::from(i) * 3600),
        })
        .collect();

    c.bench_function("filter_tcp_only_500", |b| {
        b.iter(|| {
            filter::apply(
                &entries,
                &FilterOptions {
                    tcp_only: true,
                    udp_only: false,
                    listen_only: false,
                    port: None,
                    show_all: true,
                },
            )
        });
    });
}

criterion_group!(benches, bench_filter);
criterion_main!(benches);
