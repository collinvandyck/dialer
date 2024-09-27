#![allow(unused)]

#[tokio::main]
async fn main() {
    let ips = dns_lookup::lookup_host("google.com").unwrap();
    let addr = ips.into_iter().filter(|ip| ip.is_ipv4()).next().unwrap();
    let data = [0; 8];
    let (pkt, dur) = surge_ping::ping(addr, &data).await.unwrap();
    println!("ok! pkt:{pkt:#?} dur:{dur:?}");
}
