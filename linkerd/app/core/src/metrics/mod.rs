use crate::{
    classify::{Class, SuccessOrFailure},
    control, dst, errors, http_metrics, http_metrics as metrics, opencensus, profiles,
    stack_metrics,
    svc::Param,
    telemetry, tls,
    transport::{
        self,
        labels::{TargetAddr, TlsAccept, TlsConnect},
    },
};
use linkerd_addr::Addr;
use linkerd_metrics::FmtLabels;
pub use linkerd_metrics::*;
use linkerd_server_policy as policy;
use std::{
    fmt::{self, Write},
    net::SocketAddr,
    time::{Duration, SystemTime},
};

pub type ControlHttp = http_metrics::Requests<ControlLabels, Class>;

pub type HttpEndpoint = http_metrics::Requests<EndpointLabels, Class>;

pub type HttpRoute = http_metrics::Requests<RouteLabels, Class>;

pub type HttpRouteRetry = http_metrics::Retries<RouteLabels>;

pub type Stack = stack_metrics::Registry<StackLabels>;

#[derive(Clone, Debug)]
pub struct Metrics {
    pub inbound: Inbound,
    pub outbound: Outbound,
    pub control: ControlHttp,
    pub opencensus: opencensus::metrics::Registry,
}

#[derive(Clone, Debug)]
pub struct Inbound {
    pub http_route: HttpRoute,
    pub http_endpoint: HttpEndpoint,
    pub transport: transport::Metrics,
    pub errors: errors::metrics::Inbound,
    pub stack: Stack,
}

#[derive(Clone, Debug)]
pub struct Outbound {
    pub http_route: HttpRoute,
    pub http_route_actual: HttpRoute,
    pub http_route_retry: HttpRouteRetry,
    pub http_endpoint: HttpEndpoint,
    pub transport: transport::Metrics,
    pub errors: errors::metrics::Outbound,
    pub stack: Stack,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControlLabels {
    addr: Addr,
    server_id: tls::ConditionalClientTls,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EndpointLabels {
    Inbound(InboundEndpointLabels),
    Outbound(OutboundEndpointLabels),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InboundEndpointLabels {
    pub tls: tls::ConditionalServerTls,
    pub authority: Option<http::uri::Authority>,
    pub target_addr: SocketAddr,
    pub policy: PolicyLabels,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PolicyLabels {
    pub server: policy::Labels,
    pub authz: policy::Labels,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutboundEndpointLabels {
    pub server_id: tls::ConditionalClientTls,
    pub authority: Option<http::uri::Authority>,
    pub labels: Option<String>,
    pub target_addr: SocketAddr,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StackLabels {
    pub direction: Direction,
    pub protocol: &'static str,
    pub name: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RouteLabels {
    direction: Direction,
    addr: profiles::LogicalAddr,
    labels: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Authority<'a>(&'a http::uri::Authority);

pub fn prefix_labels<'i, I>(prefix: &str, mut labels_iter: I) -> Option<String>
where
    I: Iterator<Item = (&'i String, &'i String)>,
{
    let (k0, v0) = labels_iter.next()?;
    let mut out = format!("{}_{}=\"{}\"", prefix, k0, v0);

    for (k, v) in labels_iter {
        write!(out, ",{}_{}=\"{}\"", prefix, k, v).expect("label concat must succeed");
    }
    Some(out)
}

// === impl Metrics ===

impl Metrics {
    pub fn new(retain_idle: Duration) -> (Self, impl FmtMetrics + Clone + Send + 'static) {
        let process = telemetry::process::Report::new(SystemTime::now());

        let build_info = telemetry::build_info::Report::new();

        let (control, control_report) = {
            let m = metrics::Requests::<ControlLabels, Class>::default();
            let r = m.clone().into_report(retain_idle).with_prefix("control");
            (m, r)
        };

        let (http_endpoint, endpoint_report) = {
            let m = metrics::Requests::<EndpointLabels, Class>::default();
            let r = m.clone().into_report(retain_idle);
            (m, r)
        };

        let (http_route, route_report) = {
            let m = metrics::Requests::<RouteLabels, Class>::default();
            let r = m.clone().into_report(retain_idle).with_prefix("route");
            (m, r)
        };

        let (http_route_retry, retry_report) = {
            let m = metrics::Retries::<RouteLabels>::default();
            let r = m.clone().into_report(retain_idle).with_prefix("route");
            (m, r)
        };

        let (http_route_actual, actual_report) = {
            let m = metrics::Requests::<RouteLabels, Class>::default();
            let r = m
                .clone()
                .into_report(retain_idle)
                .with_prefix("route_actual");
            (m, r.without_latencies())
        };

        let inbound_errors = errors::metrics::Inbound::default();
        let outbound_errors = errors::metrics::Outbound::default();

        let stack = stack_metrics::Registry::default();

        let (transport, transport_report) = transport::metrics::new(retain_idle);

        let (opencensus, opencensus_report) = opencensus::metrics::new();

        let metrics = Metrics {
            inbound: Inbound {
                http_endpoint: http_endpoint.clone(),
                http_route: http_route.clone(),
                transport: transport.clone().into(),
                errors: inbound_errors.clone(),
                stack: stack.clone(),
            },
            outbound: Outbound {
                http_endpoint,
                http_route,
                http_route_retry,
                http_route_actual,
                transport: transport.into(),
                errors: outbound_errors.clone(),
                stack: stack.clone(),
            },
            control,
            opencensus,
        };

        let report = inbound_errors
            .and_then(outbound_errors)
            .and_then(endpoint_report)
            .and_then(route_report)
            .and_then(retry_report)
            .and_then(actual_report)
            .and_then(control_report)
            .and_then(transport_report)
            .and_then(opencensus_report)
            .and_then(stack)
            .and_then(process)
            .and_then(build_info);

        (metrics, report)
    }
}

// === impl CtlLabels ===

impl Param<ControlLabels> for control::ControlAddr {
    fn param(&self) -> ControlLabels {
        ControlLabels {
            addr: self.addr.clone(),
            server_id: self.identity.clone(),
        }
    }
}

impl FmtLabels for ControlLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "addr=\"{}\",", self.addr)?;
        TlsConnect::from(&self.server_id).fmt_labels(f)?;

        Ok(())
    }
}

// === impl RouteLabels ===

impl Param<RouteLabels> for dst::Route {
    fn param(&self) -> RouteLabels {
        RouteLabels {
            addr: self.addr.clone(),
            direction: self.direction,
            labels: prefix_labels("rt", self.route.labels().iter()),
        }
    }
}

impl FmtLabels for RouteLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.direction.fmt_labels(f)?;
        write!(f, ",dst=\"{}\"", self.addr)?;

        if let Some(labels) = self.labels.as_ref() {
            write!(f, ",{}", labels)?;
        }

        Ok(())
    }
}

// === impl EndpointLabels ===

impl From<InboundEndpointLabels> for EndpointLabels {
    fn from(i: InboundEndpointLabels) -> Self {
        Self::Inbound(i)
    }
}

impl From<OutboundEndpointLabels> for EndpointLabels {
    fn from(i: OutboundEndpointLabels) -> Self {
        Self::Outbound(i)
    }
}

impl FmtLabels for EndpointLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inbound(i) => (Direction::In, i).fmt_labels(f),
            Self::Outbound(o) => (Direction::Out, o).fmt_labels(f),
        }
    }
}

impl FmtLabels for InboundEndpointLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(a) = self.authority.as_ref() {
            Authority(a).fmt_labels(f)?;
            write!(f, ",")?;
        }

        (TargetAddr(self.target_addr), TlsAccept::from(&self.tls)).fmt_labels(f)?;

        for (k, v) in self.policy.server.iter() {
            write!(f, ",srv_{}=\"{}\"", k, v)?;
        }
        for (k, v) in self.policy.authz.iter() {
            write!(f, ",saz_{}=\"{}\"", k, v)?;
        }

        Ok(())
    }
}

impl FmtLabels for OutboundEndpointLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(a) = self.authority.as_ref() {
            Authority(a).fmt_labels(f)?;
            write!(f, ",")?;
        }

        let ta = TargetAddr(self.target_addr);
        let tls = TlsConnect::from(&self.server_id);
        (ta, tls).fmt_labels(f)?;

        if let Some(labels) = self.labels.as_ref() {
            write!(f, ",{}", labels)?;
        }

        Ok(())
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::In => write!(f, "inbound"),
            Self::Out => write!(f, "outbound"),
        }
    }
}

impl FmtLabels for Direction {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "direction=\"{}\"", self)
    }
}

impl<'a> FmtLabels for Authority<'a> {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "authority=\"{}\"", self.0)
    }
}

impl FmtLabels for Class {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Class::Default(result) => write!(f, "classification=\"{}\"", result),
            Class::Grpc(result, status) => write!(
                f,
                "classification=\"{}\",grpc_status=\"{}\"",
                result, status
            ),
            Class::Stream(result, status) => {
                write!(f, "classification=\"{}\",error=\"{}\"", result, status)
            }
        }
    }
}

impl fmt::Display for SuccessOrFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessOrFailure::Success => write!(f, "success"),
            SuccessOrFailure::Failure => write!(f, "failure"),
        }
    }
}

// === impl StackLabels ===

impl StackLabels {
    pub fn inbound(protocol: &'static str, name: &'static str) -> Self {
        Self {
            name,
            protocol,
            direction: Direction::In,
        }
    }

    pub fn outbound(protocol: &'static str, name: &'static str) -> Self {
        Self {
            name,
            protocol,
            direction: Direction::Out,
        }
    }
}

impl FmtLabels for StackLabels {
    fn fmt_labels(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.direction.fmt_labels(f)?;
        write!(f, ",protocol=\"{}\",name=\"{}\"", self.protocol, self.name)
    }
}
