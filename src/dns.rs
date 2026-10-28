//! reqwest resolver that asks one fixed DNS server (CoreDNS) instead of
//! /etc/resolv.conf, which the NetBird sidecar takes over.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use hickory_resolver::Resolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

pub struct FixedDns(Arc<Resolver<TokioRuntimeProvider>>);

impl FixedDns {
    pub fn new(server: IpAddr) -> anyhow::Result<Self> {
        let config = ResolverConfig::from_name_servers(vec![NameServerConfig::udp_and_tcp(server)]);
        let mut opts = ResolverOpts::default();
        // Names are always FQDNs here; never append search domains.
        opts.ndots = 0;
        let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()?;
        Ok(Self(Arc::new(resolver)))
    }
}

impl Resolve for FixedDns {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.0.clone();
        Box::pin(async move {
            let mut host = name.as_str().to_string();
            if !host.ends_with('.') {
                host.push('.');
            }
            let ips = resolver.lookup_ip(host).await?;
            let addrs: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}
