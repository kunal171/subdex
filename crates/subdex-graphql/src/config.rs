//! Configuration for the GraphQL server.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Where to bind the GraphQL HTTP server and the path it serves on.
#[derive(Clone, Debug)]
pub struct GraphqlConfig {
    /// Address to bind. Defaults to `0.0.0.0:4350` (matches the port the
    /// Subsquid GraphQL server conventionally uses, easing migration).
    pub addr: SocketAddr,
    /// HTTP path the GraphQL endpoint is served on (POST for queries, GET for
    /// the GraphiQL playground). Defaults to `/graphql`.
    pub path: String,
}

impl Default for GraphqlConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 4350),
            path: "/graphql".to_string(),
        }
    }
}

impl GraphqlConfig {
    /// Bind to a specific port on all interfaces, default path.
    pub fn on_port(port: u16) -> Self {
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            ..Default::default()
        }
    }

    /// Override the served path (a leading `/` is added if missing).
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        let p = path.into();
        self.path = if p.starts_with('/') {
            p
        } else {
            format!("/{p}")
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = GraphqlConfig::default();
        assert_eq!(c.addr.port(), 4350);
        assert_eq!(c.path, "/graphql");
    }

    #[test]
    fn on_port_and_path() {
        let c = GraphqlConfig::on_port(8080).with_path("api");
        assert_eq!(c.addr.port(), 8080);
        assert_eq!(c.path, "/api", "missing leading slash is added");

        let c = GraphqlConfig::default().with_path("/q");
        assert_eq!(c.path, "/q");
    }
}
