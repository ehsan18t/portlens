//! Benchmarks for portview.
//!
//! Uses criterion for statistical benchmarking. Setup code (cloning
//! input data) is isolated from measurement via `iter_batched` so
//! results reflect only the code under test.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use portview::docker;
use portview::filter::{self, FilterOptions};
use portview::types::{PortEntry, Protocol, State};

/// Build a synthetic dataset of `n` port entries with mixed metadata.
fn synthetic_entries(n: u16) -> Vec<PortEntry> {
    (0..n)
        .map(|i| PortEntry {
            port: i,
            local_addr: if i % 2 == 0 {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            },
            proto: if i % 2 == 0 {
                Protocol::Tcp
            } else {
                Protocol::Udp
            },
            state: if i % 2 == 0 {
                State::Listen
            } else {
                State::NotApplicable
            },
            pid: 1000 + u32::from(i),
            process: Arc::from(format!("proc_{i}").as_str()),
            user: "user".into(),
            project: if i % 4 == 0 {
                Some(format!("project_{i}"))
            } else {
                None
            },
            app: if i % 5 == 0 {
                Some("Next.js".into())
            } else {
                None
            },
            uptime_secs: Some(u64::from(i) * 3600),
        })
        .collect()
}

fn bench_filter(c: &mut Criterion) {
    let entries = synthetic_entries(500);

    let tcp_only = FilterOptions {
        tcp_only: true,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: true,
    };
    c.bench_function("filter_tcp_only_500", |b| {
        b.iter_batched(
            || entries.clone(),
            |data| filter::apply(data, &tcp_only),
            BatchSize::SmallInput,
        );
    });

    let relevance = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: false,
    };
    c.bench_function("filter_relevance_500", |b| {
        b.iter_batched(
            || entries.clone(),
            |data| filter::apply(data, &relevance),
            BatchSize::SmallInput,
        );
    });

    let port_filter = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: Some(250),
        show_all: true,
    };
    c.bench_function("filter_port_500", |b| {
        b.iter_batched(
            || entries.clone(),
            |data| filter::apply(data, &port_filter),
            BatchSize::SmallInput,
        );
    });

    let combined = FilterOptions {
        tcp_only: true,
        udp_only: false,
        listen_only: true,
        port: None,
        show_all: false,
    };
    c.bench_function("filter_combined_500", |b| {
        b.iter_batched(
            || entries.clone(),
            |data| filter::apply(data, &combined),
            BatchSize::SmallInput,
        );
    });
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

criterion_group!(benches, bench_filter, bench_docker_parse);
criterion_main!(benches);
