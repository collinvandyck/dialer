#![allow(unused)]
use std::time::Duration;

fn main() {
    let ips = dns_lookup::lookup_host("google.com").unwrap();
    let addr = ips.into_iter().filter(|ip| ip.is_ipv4()).next().unwrap();
    let timeout = Duration::from_secs(20);
    let data = [8; 8];
    let opts = ping_rs::PingOptions {
        ttl: 128,
        dont_fragment: true,
    };
    let res: ping_rs::PingReply = ping_rs::send_ping(&addr, timeout, &[], Some(&opts)).unwrap();
    println!("got here");
}
